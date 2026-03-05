//! Local text embeddings for Akasha. Backend: fastembed (default, ONNX Runtime) or tract (pure Rust, Windows-friendly).

use std::path::Path;
use std::sync::RwLock;

/// Serialize embedding to bytes (little-endian f32) for storage.
pub fn embedding_to_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for &f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

/// Deserialize embedding from bytes (little-endian f32).
pub fn bytes_to_embedding(b: &[u8]) -> anyhow::Result<Vec<f32>> {
    if b.len() % 4 != 0 {
        anyhow::bail!("invalid embedding blob length");
    }
    let n = b.len() / 4;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(&b[i * 4..(i + 1) * 4]);
        out.push(f32::from_le_bytes(bytes));
    }
    Ok(out)
}

#[cfg(feature = "fastembed")]
mod fastembed_backend {
    use super::*;

    /// In-process embedder (fastembed / ONNX Runtime). Model cached under `cache_dir`.
    pub struct Embedder {
        inner: RwLock<Option<fastembed::TextEmbedding>>,
        cache_dir: std::path::PathBuf,
    }

    impl Embedder {
        pub fn new(cache_dir: impl AsRef<Path>) -> Self {
            Self {
                inner: RwLock::new(None),
                cache_dir: cache_dir.as_ref().to_path_buf(),
            }
        }

        fn ensure_loaded(&self) -> anyhow::Result<()> {
            let mut g = self.inner.write().map_err(|e| anyhow::anyhow!("lock: {}", e))?;
            if g.is_none() {
                std::fs::create_dir_all(&self.cache_dir)?;
                let opts = fastembed::InitOptions::new(fastembed::EmbeddingModel::AllMiniLML6V2)
                    .with_cache_dir(self.cache_dir.clone())
                    .with_show_download_progress(false);
                let model = fastembed::TextEmbedding::try_new(opts)
                    .map_err(|e| anyhow::anyhow!("embedding model init: {}", e))?;
                *g = Some(model);
            }
            Ok(())
        }

        pub fn embed_one(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            let vec = self.embed_slice(&[text])?;
            Ok(vec.into_iter().next().unwrap_or_default())
        }

        pub fn embed_slice(&self, texts: &[impl AsRef<str>]) -> anyhow::Result<Vec<Vec<f32>>> {
            self.ensure_loaded()?;
            let mut g = self.inner.write().map_err(|e| anyhow::anyhow!("lock: {}", e))?;
            let model = g.as_mut().ok_or_else(|| anyhow::anyhow!("embedder not loaded"))?;
            let input: Vec<&str> = texts.iter().map(|s| s.as_ref()).collect();
            let embeddings = model
                .embed(input.as_slice(), None)
                .map_err(|e| anyhow::anyhow!("embed: {}", e))?;
            let out: Vec<Vec<f32>> = embeddings.into_iter().map(|e| e.to_vec()).collect();
            Ok(out)
        }

        pub fn dimension(&self) -> usize {
            384
        }
    }
}

#[cfg(feature = "tract")]
mod tract_backend {
    use super::*;
    use ndarray::{Array, Array2};
    use std::fs::File;

    const MAX_LENGTH: usize = 256;
    const EMBED_DIM: usize = 384;
    const MODEL_HF_URL: &str =
        "https://huggingface.co/Xenova/all-MiniLM-L6-v2/resolve/main/onnx/model.onnx";
    const TOKENIZER_HF_URL: &str =
        "https://huggingface.co/Xenova/all-MiniLM-L6-v2/resolve/main/tokenizer.json";

    /// In-process embedder (tract-onnx, pure Rust). Works on Windows. Model cached under `cache_dir`.
    pub struct Embedder {
        inner: RwLock<Option<TractModel>>,
        cache_dir: std::path::PathBuf,
    }

    struct TractModel {
        model: tract_onnx::prelude::TypedRunnableModel<tract_onnx::prelude::TypedModel>,
        tokenizer: tokenizers::Tokenizer,
    }

    impl Embedder {
        pub fn new(cache_dir: impl AsRef<Path>) -> Self {
            Self {
                inner: RwLock::new(None),
                cache_dir: cache_dir.as_ref().to_path_buf(),
            }
        }

        fn ensure_loaded(&self) -> anyhow::Result<()> {
            let mut g = self.inner.write().map_err(|e| anyhow::anyhow!("lock: {}", e))?;
            if g.is_none() {
                std::fs::create_dir_all(&self.cache_dir)?;
                let model_path = self.cache_dir.join("model.onnx");
                let tokenizer_path = self.cache_dir.join("tokenizer.json");
                if !model_path.exists() || !tokenizer_path.exists() {
                    Self::download_model(&model_path, &tokenizer_path)?;
                }
                let tokenizer =
                    tokenizers::Tokenizer::from_file(&tokenizer_path).map_err(|e| anyhow::anyhow!("tokenizer: {}", e))?;
                let model = Self::load_onnx(&model_path)?;
                *g = Some(TractModel { model, tokenizer });
            }
            Ok(())
        }

        fn download_model(model_path: &Path, tokenizer_path: &Path) -> anyhow::Result<()> {
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()?;
            if !model_path.exists() {
                let mut resp = client.get(MODEL_HF_URL).send()?;
                let mut out = File::create(model_path)?;
                std::io::copy(&mut resp, &mut out)?;
            }
            if !tokenizer_path.exists() {
                let mut resp = client.get(TOKENIZER_HF_URL).send()?;
                let mut out = File::create(tokenizer_path)?;
                std::io::copy(&mut resp, &mut out)?;
            }
            Ok(())
        }

        fn load_onnx(
            path: &Path,
        ) -> anyhow::Result<tract_onnx::prelude::TypedRunnableModel<tract_onnx::prelude::TypedModel>>
        {
            use tract_onnx::prelude::*;
            let mut model = tract_onnx::onnx().model_for_path(path)?;
            model.set_input_fact(
                0,
                InferenceFact::dt_shape(i64::datum_type(), tvec!(1i64, MAX_LENGTH as i64)),
            )?;
            if model.input_outlets()?.len() > 1 {
                model.set_input_fact(
                    1,
                    InferenceFact::dt_shape(i64::datum_type(), tvec!(1i64, MAX_LENGTH as i64)),
                )?;
            }
            if model.input_outlets()?.len() > 2 {
                model.set_input_fact(
                    2,
                    InferenceFact::dt_shape(i64::datum_type(), tvec!(1i64, MAX_LENGTH as i64)),
                )?;
            }
            let model = model.into_optimized()?.into_runnable()?;
            Ok(model)
        }

        pub fn embed_one(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            let vec = self.embed_slice(&[text])?;
            Ok(vec.into_iter().next().unwrap_or_default())
        }

        pub fn embed_slice(&self, texts: &[impl AsRef<str>]) -> anyhow::Result<Vec<Vec<f32>>> {
            self.ensure_loaded()?;
            let mut g = self.inner.write().map_err(|e| anyhow::anyhow!("lock: {}", e))?;
            let m = g.as_mut().ok_or_else(|| anyhow::anyhow!("embedder not loaded"))?;
            let mut out = Vec::with_capacity(texts.len());
            for t in texts {
                let enc = m
                    .tokenizer
                    .encode(t.as_ref().to_string(), true)
                    .map_err(|e| anyhow::anyhow!("tokenize: {}", e))?;
                let ids: Vec<i64> = enc.get_ids().iter().map(|&x| x as i64).collect();
                let attn: Vec<i64> = enc.get_attention_mask().iter().map(|&x| x as i64).collect();
                let (input_ids, attention_mask) = Self::pad(ids, attn, MAX_LENGTH);
                // BERT-style models expect 3 inputs: input_ids, attention_mask, token_type_ids (zeros for single segment).
                let token_type_ids: Vec<i64> = vec![0; MAX_LENGTH];
                use tract_onnx::prelude::*;
                let input_ids_t = Array::from_shape_vec((1, MAX_LENGTH), input_ids)?;
                let attention_mask_t = Array::from_shape_vec((1, MAX_LENGTH), attention_mask)?;
                let token_type_ids_t = Array::from_shape_vec((1, MAX_LENGTH), token_type_ids)?;
                let outputs = m.model.run(tvec!(
                    input_ids_t.into_tensor().into(),
                    attention_mask_t.clone().into_tensor().into(),
                    token_type_ids_t.into_tensor().into()
                ))?;
                let last_hidden = outputs[0]
                    .to_array_view::<f32>()?
                    .into_dimensionality::<ndarray::Ix3>()?;
                let embedding = mean_pool_and_normalize(&last_hidden, &attention_mask_t);
                out.push(embedding.to_vec());
            }
            Ok(out)
        }

        fn pad(ids: Vec<i64>, attn: Vec<i64>, max_len: usize) -> (Vec<i64>, Vec<i64>) {
            let mut input_ids = vec![0i64; max_len];
            let mut attention_mask = vec![0i64; max_len];
            let len = ids.len().min(max_len);
            input_ids[..len].copy_from_slice(&ids[..len]);
            attention_mask[..len].copy_from_slice(&attn[..len]);
            (input_ids, attention_mask)
        }

        pub fn dimension(&self) -> usize {
            EMBED_DIM
        }
    }

    fn mean_pool_and_normalize(
        last_hidden: &ndarray::ArrayView3<f32>,
        attention_mask: &Array2<i64>,
    ) -> ndarray::Array1<f32> {
        let (_, seq_len, dim) = last_hidden.dim();
        let mut sum = ndarray::Array1::zeros(dim);
        let mut count = 0.0f32;
        for i in 0..seq_len {
            let w = attention_mask[[0, i]] as f32;
            count += w;
            for j in 0..dim {
                sum[j] += last_hidden[[0, i, j]] * w;
            }
        }
        if count > 0.0 {
            sum.mapv_inplace(|x| x / count);
        }
        let norm: f32 = sum.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            sum.mapv_inplace(|x| x / norm);
        }
        sum
    }
}

#[cfg(feature = "fastembed")]
pub use fastembed_backend::Embedder;

#[cfg(feature = "tract")]
pub use tract_backend::Embedder;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_embedding_bytes() {
        let v = vec![0.1f32, -0.2, 0.3];
        let b = embedding_to_bytes(&v);
        let v2 = bytes_to_embedding(&b).unwrap();
        assert_eq!(v, v2);
    }
}
