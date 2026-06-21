fn use_static_crt() -> bool {
    if cfg!(target_env = "msvc") {
        // Default /MD on Windows so llama-cpp-sys (CUDA) links cleanly. Set ESAXX_STATIC_CRT=1 for /MT.
        std::env::var("ESAXX_STATIC_CRT")
            .map(|v| v == "1")
            .unwrap_or(false)
    } else {
        true
    }
}

#[cfg(feature = "cpp")]
#[cfg(not(target_os = "macos"))]
fn main() {
    cc::Build::new()
        .cpp(true)
        .flag("-std=c++11")
        .static_crt(use_static_crt())
        .file("src/esaxx.cpp")
        .include("src")
        .compile("esaxx");
}

#[cfg(feature = "cpp")]
#[cfg(target_os = "macos")]
fn main() {
    cc::Build::new()
        .cpp(true)
        .flag("-std=c++11")
        .flag("-stdlib=libc++")
        .static_crt(use_static_crt())
        .file("src/esaxx.cpp")
        .include("src")
        .compile("esaxx");
}

#[cfg(not(feature = "cpp"))]
fn main() {}
