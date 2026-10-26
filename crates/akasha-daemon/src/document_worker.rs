//! Native parsing runs only in a short-lived child, before daemon initialization.
//! No network, shell commands, model, credentials or user memory are loaded here.
use anyhow::{ensure, Context, Result};
use base64::Engine;
use libloading::Library;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::{c_char, c_int, c_void, CStr, CString},
    path::{Path, PathBuf},
};

pub const MAX_BYTES: u64 = 25 * 1024 * 1024;
pub const MAX_PAGES: u32 = 100;
const MAX_PIXELS: u64 = 40_000_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerInput {
    pub source: PathBuf,
    pub name: String,
    pub runtime: PathBuf,
    /// inspect | extract | render
    pub operation: String,
    pub page: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page {
    pub page: u32,
    pub text: String,
    pub method: String,
    pub confidence: Option<i32>,
    pub warning: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct WorkerOutput {
    pub pages: u32,
    pub page: Option<Page>,
    pub png_base64: Option<String>,
}

pub fn runtime_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("AKASHA_OCR_RUNTIME_DIR") {
        return Ok(path.into());
    }
    Ok(std::env::current_exe()?
        .parent()
        .context("executable directory unavailable")?
        .join("ocr-runtime"))
}
fn sha(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut input = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut bytes = [0; 65536];
    loop {
        let count = input.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        hasher.update(&bytes[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
pub fn verify_runtime(path: &Path) -> Result<()> {
    let lock = path.join("runtime-lock.json");
    let compiled = option_env!("AKASHA_OCR_LOCK_SHA256").filter(|v| !v.is_empty());
    let expected = if cfg!(feature = "consumer-beta") {
        compiled.map(str::to_string)
    } else {
        compiled
            .map(str::to_string)
            .or_else(|| std::env::var("AKASHA_OCR_LOCK_SHA256").ok())
    };
    ensure!(
        expected.as_deref() == Some(sha(&lock)?.as_str()),
        "OCR runtime non qualifié : empreinte du paquet absente ou modifiée"
    );
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(lock)?)?;
    let pins: serde_json::Value =
        serde_json::from_str(include_str!("../../../spec/ocr_runtime.json"))?;
    ensure!(
        value["schema_version"] == 1 && value["pins"] == pins,
        "OCR runtime version mismatch"
    );
    let files = value["files"]
        .as_array()
        .context("OCR runtime file inventory missing")?;
    for required in [
        "pdfium.dll",
        "tesseract55.dll",
        "tessdata/fra.traineddata",
        "tessdata/eng.traineddata",
        "tessdata/osd.traineddata",
    ] {
        ensure!(
            files.iter().any(|row| row["path"] == required),
            "OCR component missing: {required}"
        );
    }
    for row in files {
        let relative = Path::new(row["path"].as_str().context("OCR file path missing")?);
        ensure!(
            !relative.is_absolute()
                && relative
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_))),
            "unsafe OCR path"
        );
        ensure!(
            row["sha256"].as_str() == Some(sha(&path.join(relative))?.as_str()),
            "OCR component altered: {}",
            relative.display()
        );
    }
    Ok(())
}

struct Handle {
    ptr: *mut c_void,
    close: unsafe extern "C" fn(*mut c_void),
}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            (self.close)(self.ptr);
        }
    }
}
struct Native {
    pdf: Library,
    tess: Library,
}
// Load only these pinned absolute DLLs and their OS dependencies, never CWD/PATH DLLs.
unsafe fn load(path: &Path) -> Result<Library> {
    #[cfg(windows)]
    {
        Ok(libloading::os::windows::Library::load_with_flags(path, 0x1100)?.into())
    }
    #[cfg(not(windows))]
    {
        Ok(Library::new(path)?)
    }
}
macro_rules! native {
    ($lib:expr, $name:literal, $kind:ty) => {
        *$lib.get::<$kind>(concat!($name, "\0").as_bytes())?
    };
}
impl Native {
    unsafe fn new(path: &Path) -> Result<Self> {
        verify_runtime(path)?;
        let pdf = load(&path.join("pdfium.dll"))?;
        let tess = load(&path.join("tesseract55.dll"))?;
        let version = native!(tess, "TessVersion", unsafe extern "C" fn() -> *const c_char);
        ensure!(
            CStr::from_ptr(version()).to_bytes() == b"5.5.3",
            "Tesseract version mismatch"
        );
        let init = native!(pdf, "FPDF_InitLibrary", unsafe extern "C" fn());
        init();
        Ok(Self { pdf, tess })
    }
    unsafe fn ocr(
        &self,
        pixels: &[u8],
        width: u32,
        height: u32,
        runtime: &Path,
    ) -> Result<(String, i32)> {
        let create = native!(
            self.tess,
            "TessBaseAPICreate",
            unsafe extern "C" fn() -> *mut c_void
        );
        let ptr = create();
        ensure!(!ptr.is_null(), "Tesseract initialization failed");
        let api = Handle {
            ptr,
            close: native!(
                self.tess,
                "TessBaseAPIDelete",
                unsafe extern "C" fn(*mut c_void)
            ),
        };
        let directory = CString::new(runtime.join("tessdata").to_string_lossy().as_bytes())?;
        let init = native!(
            self.tess,
            "AkashaTessInitUtf8",
            unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int
        );
        ensure!(
            init(api.ptr, directory.as_ptr(), c"fra+eng".as_ptr()) == 0,
            "OCR languages unavailable"
        );
        let mode = native!(
            self.tess,
            "TessBaseAPISetPageSegMode",
            unsafe extern "C" fn(*mut c_void, c_int)
        );
        mode(api.ptr, 1); // AUTO_OSD
        let image = native!(
            self.tess,
            "TessBaseAPISetImage",
            unsafe extern "C" fn(*mut c_void, *const u8, c_int, c_int, c_int, c_int)
        );
        image(
            api.ptr,
            pixels.as_ptr(),
            width as i32,
            height as i32,
            1,
            width as i32,
        );
        let dpi = native!(
            self.tess,
            "TessBaseAPISetSourceResolution",
            unsafe extern "C" fn(*mut c_void, c_int)
        );
        dpi(api.ptr, 300);
        let recognize = native!(
            self.tess,
            "TessBaseAPIRecognize",
            unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int
        );
        ensure!(
            recognize(api.ptr, std::ptr::null_mut()) == 0,
            "OCR recognition failed"
        );
        let text = native!(
            self.tess,
            "TessBaseAPIGetUTF8Text",
            unsafe extern "C" fn(*mut c_void) -> *mut c_char
        )(api.ptr);
        ensure!(!text.is_null(), "OCR returned no text");
        let owned = CStr::from_ptr(text).to_string_lossy().into_owned();
        native!(
            self.tess,
            "TessDeleteText",
            unsafe extern "C" fn(*mut c_char)
        )(text);
        ensure!(owned.len() <= 2 * 1024 * 1024, "OCR page text too large");
        let confidence = native!(
            self.tess,
            "TessBaseAPIMeanTextConf",
            unsafe extern "C" fn(*mut c_void) -> c_int
        )(api.ptr);
        Ok((owned, confidence))
    }
    unsafe fn pdf(&self, input: &WorkerInput, bytes: &[u8]) -> Result<WorkerOutput> {
        let open = native!(
            self.pdf,
            "FPDF_LoadMemDocument64",
            unsafe extern "C" fn(*const c_void, usize, *const c_char) -> *mut c_void
        );
        let ptr = open(bytes.as_ptr().cast(), bytes.len(), std::ptr::null());
        ensure!(
            !ptr.is_null(),
            "PDF illisible, endommagé ou protégé par mot de passe"
        );
        let doc = Handle {
            ptr,
            close: native!(
                self.pdf,
                "FPDF_CloseDocument",
                unsafe extern "C" fn(*mut c_void)
            ),
        };
        let count = native!(
            self.pdf,
            "FPDF_GetPageCount",
            unsafe extern "C" fn(*mut c_void) -> c_int
        )(doc.ptr);
        ensure!(
            count > 0 && count <= MAX_PAGES as i32,
            "Limite de 100 pages par fichier dépassée, ou PDF vide"
        );
        if input.operation == "inspect" {
            return Ok(WorkerOutput {
                pages: count as u32,
                page: None,
                png_base64: None,
            });
        }
        ensure!(
            input.page > 0 && input.page <= count as u32,
            "invalid page number"
        );
        let ptr = native!(
            self.pdf,
            "FPDF_LoadPage",
            unsafe extern "C" fn(*mut c_void, c_int) -> *mut c_void
        )(doc.ptr, input.page as i32 - 1);
        ensure!(!ptr.is_null(), "PDF page unreadable");
        let page = Handle {
            ptr,
            close: native!(
                self.pdf,
                "FPDF_ClosePage",
                unsafe extern "C" fn(*mut c_void)
            ),
        };
        let width = native!(
            self.pdf,
            "FPDF_GetPageWidthF",
            unsafe extern "C" fn(*mut c_void) -> f32
        )(page.ptr);
        let height = native!(
            self.pdf,
            "FPDF_GetPageHeightF",
            unsafe extern "C" fn(*mut c_void) -> f32
        )(page.ptr);
        ensure!(
            width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0,
            "invalid PDF page dimensions"
        );
        let text = self.pdf_text(page.ptr)?;
        let objects = native!(
            self.pdf,
            "FPDFPage_CountObjects",
            unsafe extern "C" fn(*mut c_void) -> c_int
        )(page.ptr);
        let mut large_image = false;
        for index in 0..objects.min(100000) {
            let object = native!(
                self.pdf,
                "FPDFPage_GetObject",
                unsafe extern "C" fn(*mut c_void, c_int) -> *mut c_void
            )(page.ptr, index);
            let kind = native!(
                self.pdf,
                "FPDFPageObj_GetType",
                unsafe extern "C" fn(*mut c_void) -> c_int
            )(object);
            if kind == 3 || kind == 5 {
                let (mut left, mut bottom, mut right, mut top) = (0f32, 0f32, 0f32, 0f32);
                if native!(
                    self.pdf,
                    "FPDFPageObj_GetBounds",
                    unsafe extern "C" fn(
                        *mut c_void,
                        *mut f32,
                        *mut f32,
                        *mut f32,
                        *mut f32,
                    ) -> c_int
                )(object, &mut left, &mut bottom, &mut right, &mut top)
                    != 0
                    && (right - left) * (top - bottom) > width * height * 0.4
                {
                    large_image = true;
                }
            }
        }
        let needs_ocr =
            text.trim().chars().count() < 10 || (large_image && text.chars().count() < 200);
        if input.operation == "extract" && !needs_ocr {
            return Ok(WorkerOutput {
                pages: count as u32,
                page: Some(Page {
                    page: input.page,
                    text,
                    method: "pdfium_text".into(),
                    confidence: None,
                    warning: None,
                }),
                png_base64: None,
            });
        }
        let scale = if input.operation == "render" {
            (1600.0 / width.max(height)).min(2.0)
        } else {
            300.0 / 72.0
        };
        let (w, h) = (
            (width * scale).ceil() as u32,
            (height * scale).ceil() as u32,
        );
        ensure!(
            w > 0 && h > 0 && u64::from(w) * u64::from(h) <= MAX_PIXELS,
            "PDF page exceeds rendering pixel limit"
        );
        let bitmap = native!(
            self.pdf,
            "FPDFBitmap_Create",
            unsafe extern "C" fn(c_int, c_int, c_int) -> *mut c_void
        )(w as i32, h as i32, 0);
        ensure!(!bitmap.is_null(), "PDF page allocation failed");
        let bitmap = Handle {
            ptr: bitmap,
            close: native!(
                self.pdf,
                "FPDFBitmap_Destroy",
                unsafe extern "C" fn(*mut c_void)
            ),
        };
        native!(
            self.pdf,
            "FPDFBitmap_FillRect",
            unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int, c_int, u32) -> c_int
        )(bitmap.ptr, 0, 0, w as i32, h as i32, 0xffffffff);
        native!(
            self.pdf,
            "FPDF_RenderPageBitmap",
            unsafe extern "C" fn(
                *mut c_void,
                *mut c_void,
                c_int,
                c_int,
                c_int,
                c_int,
                c_int,
                c_int,
            )
        )(bitmap.ptr, page.ptr, 0, 0, w as i32, h as i32, 0, 1);
        let buffer = native!(
            self.pdf,
            "FPDFBitmap_GetBuffer",
            unsafe extern "C" fn(*mut c_void) -> *mut c_void
        )(bitmap.ptr);
        let stride = native!(
            self.pdf,
            "FPDFBitmap_GetStride",
            unsafe extern "C" fn(*mut c_void) -> c_int
        )(bitmap.ptr);
        ensure!(
            !buffer.is_null() && stride >= w as i32 * 4,
            "invalid PDF bitmap"
        );
        let pixels = std::slice::from_raw_parts(buffer.cast::<u8>(), stride as usize * h as usize);
        let mut gray = Vec::with_capacity(w as usize * h as usize);
        for row in pixels.chunks(stride as usize) {
            for p in row[..w as usize * 4].chunks_exact(4) {
                gray.push(
                    ((u32::from(p[0]) * 29 + u32::from(p[1]) * 150 + u32::from(p[2]) * 77) >> 8)
                        as u8,
                );
            }
        }
        if input.operation == "render" {
            return Ok(WorkerOutput {
                pages: count as u32,
                page: None,
                png_base64: Some(png(&gray, w, h)?),
            });
        }
        let (text, confidence) = self.ocr(&gray, w, h, &input.runtime)?;
        Ok(WorkerOutput {
            pages: count as u32,
            page: Some(ocr_page(input.page, text, confidence)),
            png_base64: None,
        })
    }
    unsafe fn pdf_text(&self, page: *mut c_void) -> Result<String> {
        let ptr = native!(
            self.pdf,
            "FPDFText_LoadPage",
            unsafe extern "C" fn(*mut c_void) -> *mut c_void
        )(page);
        ensure!(!ptr.is_null(), "PDF text extraction failed");
        let text = Handle {
            ptr,
            close: native!(
                self.pdf,
                "FPDFText_ClosePage",
                unsafe extern "C" fn(*mut c_void)
            ),
        };
        let count = native!(
            self.pdf,
            "FPDFText_CountChars",
            unsafe extern "C" fn(*mut c_void) -> c_int
        )(text.ptr);
        ensure!(
            (0..=2_000_000).contains(&count),
            "PDF text exceeds page limit"
        );
        let mut utf16 = vec![0u16; count as usize + 1];
        let read = native!(
            self.pdf,
            "FPDFText_GetText",
            unsafe extern "C" fn(*mut c_void, c_int, c_int, *mut u16) -> c_int
        )(text.ptr, 0, count, utf16.as_mut_ptr());
        ensure!(
            read >= 0 && read as usize <= utf16.len(),
            "PDF text length invalid"
        );
        utf16.truncate(read as usize);
        if utf16.last() == Some(&0) {
            utf16.pop();
        }
        Ok(String::from_utf16_lossy(&utf16))
    }
}
fn ocr_page(page: u32, text: String, confidence: i32) -> Page {
    let warning = if text.trim().is_empty() {
        Some("Page non lue : aucun texte reconnu".into())
    } else if confidence < 65 {
        Some(format!(
            "OCR incertain (confiance {confidence} %) : vérifiez la page originale"
        ))
    } else {
        None
    };
    Page {
        page,
        text,
        method: "tesseract_5_fra_eng".into(),
        confidence: Some(confidence),
        warning,
    }
}
fn png(gray: &[u8], w: u32, h: u32) -> Result<String> {
    use image::ImageEncoder;
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes).write_image(
        gray,
        w,
        h,
        image::ExtendedColorType::L8,
    )?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}
pub fn execute(input: &WorkerInput) -> Result<WorkerOutput> {
    ensure!(
        matches!(input.operation.as_str(), "inspect" | "extract" | "render"),
        "invalid worker operation"
    );
    ensure!(
        std::fs::metadata(&input.source)?.len() <= MAX_BYTES,
        "Limite de 25 Mio par fichier dépassée"
    );
    let bytes = std::fs::read(&input.source)?;
    let extension = Path::new(&input.name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(extension.as_str(), "txt" | "md") {
        ensure!(
            input.operation != "render",
            "Le texte original est accessible dans la source"
        );
        let text = String::from_utf8(bytes).context("Document texte non UTF-8")?;
        return Ok(WorkerOutput {
            pages: 1,
            page: if input.operation == "inspect" {
                None
            } else {
                Some(Page {
                    page: 1,
                    text,
                    method: "utf8_text".into(),
                    confidence: None,
                    warning: None,
                })
            },
            png_base64: None,
        });
    }
    if bytes.starts_with(b"%PDF-") {
        return unsafe { Native::new(&input.runtime)?.pdf(input, &bytes) };
    }
    let extension = Path::new(&input.name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    ensure!(
        matches!(extension.as_str(), "jpg" | "jpeg" | "png"),
        "unsupported binary document format"
    );
    if input.operation == "inspect" {
        return Ok(WorkerOutput {
            pages: 1,
            page: None,
            png_base64: None,
        });
    }
    ensure!(input.page == 1, "invalid image page");
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(12000);
    limits.max_image_height = Some(12000);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader.decode()?;
    ensure!(
        u64::from(decoded.width()) * u64::from(decoded.height()) <= MAX_PIXELS,
        "Image exceeds pixel limit"
    );
    let image = decoded.to_luma8();
    if input.operation == "render" {
        return Ok(WorkerOutput {
            pages: 1,
            page: None,
            png_base64: Some(png(&image, image.width(), image.height())?),
        });
    }
    let (text, confidence) = unsafe {
        Native::new(&input.runtime)?.ocr(&image, image.width(), image.height(), &input.runtime)?
    };
    Ok(WorkerOutput {
        pages: 1,
        page: Some(ocr_page(1, text, confidence)),
        png_base64: None,
    })
}
/// Returns Some(exit code) only in worker mode; callers must exit immediately.
pub fn entry() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--document-worker")) {
        return None;
    }
    let result = (|| -> Result<WorkerOutput> {
        let path = PathBuf::from(args.next().context("worker configuration missing")?);
        ensure!(
            std::fs::metadata(&path)?.len() <= 65536,
            "worker configuration too large"
        );
        let input: WorkerInput = serde_json::from_slice(&std::fs::read(path)?)?;
        execute(&input)
    })();
    match result {
        Ok(value) => {
            println!(
                "{}",
                serde_json::to_string(&value).expect("serializable worker result")
            );
            Some(0)
        }
        Err(error) => {
            eprintln!("{error:#}");
            Some(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn low_confidence_and_blank_pages_never_silently_succeed() {
        assert!(ocr_page(1, String::new(), 0)
            .warning
            .unwrap()
            .contains("non lue"));
        assert!(ocr_page(2, "Montant 42".into(), 30)
            .warning
            .unwrap()
            .contains("incertain"));
        assert!(ocr_page(1, "Montant 42".into(), 95).warning.is_none());
    }
    #[test]
    fn oversized_input_is_rejected_before_native_loading() {
        let file = tempfile::NamedTempFile::new().unwrap();
        file.as_file().set_len(MAX_BYTES + 1).unwrap();
        let error = execute(&WorkerInput {
            source: file.path().into(),
            name: "large.pdf".into(),
            runtime: PathBuf::new(),
            operation: "inspect".into(),
            page: 0,
        })
        .unwrap_err();
        assert!(error.to_string().contains("25 Mio"));
    }
}
