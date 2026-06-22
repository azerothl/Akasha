//! Host hardware detection for embedded profile tier classification.

use serde::Serialize;

/// Snapshot of the host machine relevant to embedded inference.
#[derive(Debug, Clone, Serialize)]
pub struct HardwareProfile {
    pub ram_gb: u32,
    pub vram_mb: Option<u32>,
    pub gpu_name: Option<String>,
    pub cuda_compiled: bool,
    pub cuda_runtime: bool,
    pub cpu_model: Option<String>,
    pub tier_id: String,
}

/// Detect hardware and assign a static profile tier.
pub fn detect_hardware() -> HardwareProfile {
    let ram_gb = detect_ram_gb();
    let (gpu_name, vram_mb) = detect_gpu_vram();
    let cuda_compiled = crate::config::llama_cpp_compiled()
        && cfg!(any(feature = "llama-cpp-cuda", feature = "llama-cpp-metal"));
    let cuda_runtime = cuda_compiled && cuda_runtime_available();
    let cpu_model = detect_cpu_model();
    let tier_id = classify_tier(ram_gb, vram_mb, cuda_compiled, cuda_runtime);
    HardwareProfile {
        ram_gb,
        vram_mb,
        gpu_name,
        cuda_compiled,
        cuda_runtime,
        cpu_model,
        tier_id,
    }
}

/// Classify tier from raw metrics (testable without subprocesses).
pub fn classify_tier(
    ram_gb: u32,
    vram_mb: Option<u32>,
    cuda_compiled: bool,
    cuda_runtime: bool,
) -> String {
    if ram_gb >= 32 && (!cuda_runtime || vram_mb.unwrap_or(0) < 2048) {
        return "cpu_capable_32gb".to_string();
    }
    if !cuda_compiled || !cuda_runtime || vram_mb.is_none() {
        return "cpu_only".to_string();
    }
    let vram = vram_mb.unwrap_or(0);
    if vram <= 6144 {
        "gpu_low_4gb".to_string()
    } else if vram <= 10240 {
        "gpu_mid_8gb".to_string()
    } else {
        "gpu_high_12gb".to_string()
    }
}

fn detect_ram_gb() -> u32 {
    if let Ok(v) = std::env::var("AKASHA_HOST_RAM_GB") {
        if let Ok(n) = v.trim().parse::<u32>() {
            if n > 0 {
                return n;
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(raw) = std::fs::read_to_string("/proc/meminfo") {
            for line in raw.lines() {
                if line.starts_with("MemTotal:") {
                    if let Some(kb) = line
                        .split_whitespace()
                        .nth(1)
                        .and_then(|s| s.parse::<u64>().ok())
                    {
                        return ((kb + 512 * 1024) / (1024 * 1024)).max(1) as u32;
                    }
                }
            }
        }
    }
    #[cfg(windows)]
    {
        if let Ok(out) = std::process::Command::new("wmic")
            .args(["computersystem", "get", "totalphysicalmemory", "/value"])
            .output()
        {
            if out.status.success() {
                for line in String::from_utf8_lossy(&out.stdout).lines() {
                    if let Some(bytes) = line.strip_prefix("TotalPhysicalMemory=") {
                        if let Ok(b) = bytes.trim().parse::<u64>() {
                            return ((b + 512 * 1024 * 1024) / (1024 * 1024 * 1024)).max(1) as u32;
                        }
                    }
                }
            }
        }
    }
    16
}

fn detect_gpu_vram() -> (Option<String>, Option<u32>) {
    let out = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output();
    if let Ok(out) = out {
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            if let Some(line) = text.lines().find(|l| !l.trim().is_empty()) {
                let parts: Vec<&str> = line.split(',').map(str::trim).collect();
                if parts.len() >= 2 {
                    let name = parts[0].to_string();
                    let vram = parts[1].parse::<u32>().ok();
                    return (Some(name), vram);
                }
            }
        }
    }
    (None, None)
}

fn detect_cpu_model() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(raw) = std::fs::read_to_string("/proc/cpuinfo") {
            for line in raw.lines() {
                if line.starts_with("model name") {
                    if let Some((_k, v)) = line.split_once(':') {
                        return Some(v.trim().to_string());
                    }
                }
            }
        }
    }
    #[cfg(windows)]
    {
        if let Ok(out) = std::process::Command::new("wmic")
            .args(["cpu", "get", "name", "/value"])
            .output()
        {
            if out.status.success() {
                for line in String::from_utf8_lossy(&out.stdout).lines() {
                    if let Some(name) = line.strip_prefix("Name=") {
                        let t = name.trim();
                        if !t.is_empty() {
                            return Some(t.to_string());
                        }
                    }
                }
            }
        }
    }
    None
}

#[cfg(feature = "llama-cpp-cuda")]
fn cuda_runtime_available() -> bool {
    llama_cpp_4::supports_gpu_offload()
}

#[cfg(all(feature = "llama-cpp", not(feature = "llama-cpp-cuda")))]
fn cuda_runtime_available() -> bool {
    false
}

#[cfg(not(feature = "llama-cpp"))]
fn cuda_runtime_available() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_cpu_only_without_cuda() {
        assert_eq!(classify_tier(16, None, false, false), "cpu_only");
    }

    #[test]
    fn tier_gpu_low_4gb() {
        assert_eq!(classify_tier(32, Some(4096), true, true), "gpu_low_4gb");
    }

    #[test]
    fn tier_cpu_capable_32gb_weak_gpu() {
        assert_eq!(classify_tier(32, None, true, false), "cpu_capable_32gb");
    }

    #[test]
    fn tier_gpu_mid_8gb() {
        assert_eq!(classify_tier(16, Some(8192), true, true), "gpu_mid_8gb");
    }

    #[test]
    fn tier_gpu_high_12gb() {
        assert_eq!(classify_tier(64, Some(24576), true, true), "gpu_high_12gb");
    }
}
