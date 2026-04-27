//! Interactive PTY sessions (Hermes tranche 1). See `spec/43_session_terminal.md`.
//!
//! HTTP surface: `/api/terminal/pty/sessions` (wired in `api.rs`).

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex, OnceLock};

const OUTPUT_CAP_BYTES: usize = 512 * 1024;

#[derive(Debug, Deserialize)]
pub struct PtyCreateBody {
    /// Full argv including program (e.g. `["/bin/bash","-l"]`). If omitted, default shell.
    #[serde(default)]
    pub argv: Option<Vec<String>>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default = "default_cols")]
    pub cols: u16,
    #[serde(default = "default_rows")]
    pub rows: u16,
}

fn default_cols() -> u16 {
    80
}
fn default_rows() -> u16 {
    24
}

#[derive(Debug, Serialize)]
pub struct PtyCreateResponse {
    pub session_id: String,
}

#[derive(Debug, Deserialize)]
pub struct PtyInputBody {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub bytes_b64: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PtyResizeBody {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Serialize)]
pub struct PtyOutputResponse {
    pub data_b64: String,
    pub bytes: usize,
}

pub struct PtyManager {
    sessions: Mutex<HashMap<String, Arc<PtySessionInner>>>,
}

pub struct PtySessionInner {
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    out: Arc<Mutex<VecDeque<u8>>>,
    child: Arc<Mutex<Option<Box<dyn Child + Send + Sync>>>>,
    reader: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl PtyManager {
    pub fn global() -> &'static Self {
        static G: OnceLock<PtyManager> = OnceLock::new();
        G.get_or_init(|| PtyManager {
            sessions: Mutex::new(HashMap::new()),
        })
    }

    pub fn create(&self, body: PtyCreateBody) -> anyhow::Result<PtyCreateResponse> {
        let system = native_pty_system();
        let size = PtySize {
            rows: body.rows,
            cols: body.cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = system.openpty(size)?;
        let cmd = build_command(&body)?;
        let child_box = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);

        let master = Arc::new(Mutex::new(pair.master));
        let out = Arc::new(Mutex::new(VecDeque::new()));

        let r = {
            let g = master
                .lock()
                .map_err(|e| anyhow::anyhow!("pty master lock poisoned: {}", e))?;
            g.try_clone_reader()?
        };
        let out_thread = Arc::clone(&out);
        let reader_handle = std::thread::spawn(move || {
            let mut r = r;
            let mut buf = [0u8; 4096];
            loop {
                match r.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let Ok(mut q) = out_thread.lock() else {
                            break;
                        };
                        for &b in &buf[..n] {
                            q.push_back(b);
                            while q.len() > OUTPUT_CAP_BYTES {
                                q.pop_front();
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let writer = {
            let g = master
                .lock()
                .map_err(|e| anyhow::anyhow!("pty master lock poisoned: {}", e))?;
            Arc::new(Mutex::new(g.take_writer()?))
        };

        let session_id = uuid::Uuid::new_v4().to_string();
        let child_holder: Arc<Mutex<Option<Box<dyn Child + Send + Sync>>>> =
            Arc::new(Mutex::new(Some(child_box)));

        let inner = Arc::new(PtySessionInner {
            master,
            writer,
            out,
            child: Arc::clone(&child_holder),
            reader: Mutex::new(Some(reader_handle)),
        });

        self.sessions
            .lock()
            .map_err(|e| anyhow::anyhow!("pty sessions lock poisoned: {}", e))?
            .insert(session_id.clone(), inner);

        Ok(PtyCreateResponse { session_id })
    }

    pub fn write_input(&self, id: &str, body: PtyInputBody) -> anyhow::Result<()> {
        let inner = self.get_arc(id)?;
        let mut data = Vec::new();
        if let Some(t) = body.text {
            data.extend_from_slice(t.as_bytes());
        }
        if let Some(b64) = body.bytes_b64 {
            use base64::Engine;
            data.extend_from_slice(
                &base64::engine::general_purpose::STANDARD
                    .decode(b64.trim())
                    .map_err(|e| anyhow::anyhow!("invalid base64: {}", e))?,
            );
        }
        if data.is_empty() {
            anyhow::bail!("empty input: set text or bytes_b64");
        }
        let mut w = inner
            .writer
            .lock()
            .map_err(|e| anyhow::anyhow!("pty writer lock poisoned: {}", e))?;
        w.write_all(&data)?;
        w.flush()?;
        Ok(())
    }

    pub fn read_output(&self, id: &str, max: usize) -> anyhow::Result<PtyOutputResponse> {
        let inner = self.get_arc(id)?;
        let max = max.clamp(1, OUTPUT_CAP_BYTES);
        let mut chunk = Vec::with_capacity(max);
        {
            let mut q = inner
                .out
                .lock()
                .map_err(|e| anyhow::anyhow!("pty out lock poisoned: {}", e))?;
            while chunk.len() < max && !q.is_empty() {
                if let Some(b) = q.pop_front() {
                    chunk.push(b);
                }
            }
        }
        use base64::Engine;
        let data_b64 = base64::engine::general_purpose::STANDARD.encode(&chunk);
        let bytes = chunk.len();
        Ok(PtyOutputResponse { data_b64, bytes })
    }

    pub fn resize(&self, id: &str, body: PtyResizeBody) -> anyhow::Result<()> {
        let inner = self.get_arc(id)?;
        let g = inner
            .master
            .lock()
            .map_err(|e| anyhow::anyhow!("pty master lock poisoned: {}", e))?;
        g.resize(PtySize {
            rows: body.rows,
            cols: body.cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }

    pub fn close(&self, id: &str) -> anyhow::Result<()> {
        let inner = {
            let mut map = self
                .sessions
                .lock()
                .map_err(|e| anyhow::anyhow!("pty sessions lock poisoned: {}", e))?;
            map.remove(id)
                .ok_or_else(|| anyhow::anyhow!("unknown session_id"))?
        };
        if let Ok(mut c) = inner.child.lock() {
            if let Some(mut ch) = c.take() {
                let _ = ch.kill();
                let _ = ch.wait();
            }
        }
        if let Ok(mut h) = inner.reader.lock() {
            if let Some(j) = h.take() {
                let _ = j.join();
            }
        }
        Ok(())
    }

    fn get_arc(&self, id: &str) -> anyhow::Result<Arc<PtySessionInner>> {
        let map = self
            .sessions
            .lock()
            .map_err(|e| anyhow::anyhow!("pty sessions lock poisoned: {}", e))?;
        map.get(id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown session_id"))
    }
}

fn build_command(body: &PtyCreateBody) -> anyhow::Result<CommandBuilder> {
    if let Some(argv) = &body.argv {
        if argv.is_empty() {
            anyhow::bail!("argv must not be empty when provided");
        }
        let mut cb = CommandBuilder::new(&argv[0]);
        for a in argv.iter().skip(1) {
            cb.arg(a);
        }
        if let Some(ref cwd) = body.cwd {
            cb.cwd(cwd);
        }
        Ok(cb)
    } else {
        default_shell_command(body.cwd.as_deref())
    }
}

fn default_shell_command(cwd: Option<&str>) -> anyhow::Result<CommandBuilder> {
    #[cfg(windows)]
    {
        let exe = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into());
        let mut cb = CommandBuilder::new(&exe);
        cb.arg("/K");
        if let Some(c) = cwd {
            cb.cwd(c);
        }
        Ok(cb)
    }
    #[cfg(unix)]
    {
        let sh = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let mut cb = CommandBuilder::new(&sh);
        cb.arg("-i");
        if let Some(c) = cwd {
            cb.cwd(c);
        }
        Ok(cb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(
        target_os = "windows",
        ignore = "default akasha-daemon test graph may fail to link on Windows (ONNX/embeddings); run on Linux CI or use --no-default-features"
    )]
    fn pty_echo_smoke() {
        let mgr = PtyManager {
            sessions: Mutex::new(HashMap::new()),
        };
        #[cfg(unix)]
        let body = PtyCreateBody {
            argv: Some(vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf 'PTY_OK'; exit 0".into(),
            ]),
            cwd: None,
            cols: 80,
            rows: 24,
        };
        #[cfg(windows)]
        let body = PtyCreateBody {
            argv: Some(vec![
                "cmd.exe".into(),
                "/c".into(),
                "echo PTY_OK".into(),
            ]),
            cwd: None,
            cols: 80,
            rows: 24,
        };
        let res = mgr.create(body).expect("create");
        std::thread::sleep(std::time::Duration::from_millis(400));
        let out = mgr
            .read_output(&res.session_id, 4096)
            .expect("read_output");
        mgr.close(&res.session_id).expect("close");
        assert!(!out.data_b64.is_empty());
        use base64::Engine;
        let raw = base64::engine::general_purpose::STANDARD
            .decode(&out.data_b64)
            .unwrap();
        let s = String::from_utf8_lossy(&raw);
        assert!(
            s.contains("PTY_OK"),
            "expected PTY_OK in {:?}, got {:?}",
            s,
            s
        );
    }
}
