//! Local text embeddings for Akasha. Uses fastembed (ONNX in-process); model is cached under
//! the provided cache_dir (e.g. data_dir/embedding_model) so no third-party application is required.

use std::path::Path;
use std::sync::RwLock;

/// In-process embedder. Model is loaded on first use and cached under `cache_dir`.
pub struct Embedder {
    inner: RwLock<Option<fastembed::TextEmbedding>>,
    cache_dir: std::path::PathBuf,
}

impl Embedder {
    /// Create an embedder that will cache the model under `cache_dir` (e.g. data_dir/embedding_model).
    /// The model is downloaded on first embed() if not already present; inference runs in-process.
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

    /// Embed a single text. Returns a vector of dimension 384 for AllMiniLML6V2.
    pub fn embed_one(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let vec = self.embed_slice(&[text])?;
        Ok(vec.into_iter().next().unwrap_or_default())
    }

    /// Embed multiple texts. Order is preserved.
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

    /// Dimension of the embedding vectors (384 for AllMiniLML6V2).
    pub fn dimension(&self) -> usize {
        384
    }
}

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
