//! Control-сокет демона для GUI/CLI: `$XDG_RUNTIME_DIR/opendictate.sock`.
//!
//! Протокол: одна JSON-строка с `\n` → одна JSON-строка ответа.
//! Запросы:
//!   {"status":true}
//!   {"set":{"language":"en"}} | {"set":{"model":"small"}}
//!   {"set":{"auto_paste":false}} | {"set":{"prompt":"..."}}
//!
//! Смена модели применяется главным циклом при следующей фразе (хот-релоад).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct Shared {
    pub lang: Arc<Mutex<String>>,
    pub model: Arc<Mutex<String>>,
    pub loaded_model: Arc<Mutex<String>>,
    pub auto_paste: Arc<Mutex<bool>>,
    pub prompt: Arc<Mutex<String>>,
    pub recording: Arc<Mutex<bool>>,
    pub last: Arc<Mutex<(String, u128)>>,
}

impl Shared {
    pub fn snapshot(&self) -> serde_json::Value {
        let get = |m: &Arc<Mutex<String>>| m.lock().map(|v| v.clone()).unwrap_or_default();
        serde_json::json!({
            "ok": true,
            "language": get(&self.lang),
            "model": get(&self.model),
            "loaded_model": get(&self.loaded_model),
            "auto_paste": self.auto_paste.lock().map(|v| *v).unwrap_or(false),
            "recording": self.recording.lock().map(|v| *v).unwrap_or(false),
            "last_text": self.last.lock().map(|v| v.0.clone()).unwrap_or_default(),
            "last_ms": self.last.lock().map(|v| v.1).unwrap_or(0),
        })
    }
}

#[derive(Debug, Deserialize)]
struct Set {
    language: Option<String>,
    model: Option<String>,
    auto_paste: Option<bool>,
    prompt: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct Req {
    status: Option<bool>,
    set: Option<Set>,
}

#[derive(Serialize)]
struct ErrResp {
    ok: bool,
    error: String,
}

pub fn sock_path() -> PathBuf {
    if let Ok(rt) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(rt).join("opendictate.sock");
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "def".into());
    PathBuf::from(format!("/tmp/opendictate-{user}.sock"))
}

/// Блокирующий цикл в отдельном треде.
pub fn serve(st: Shared) -> Result<()> {
    let path = sock_path();
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    eprintln!("[ctl] control: {}", path.display());
    for stream in listener.incoming() {
        let Ok(mut s) = stream else { continue };
        let st = st.clone();
        std::thread::spawn(move || {
            let mut line = String::new();
            let reader = match s.try_clone() {
                Ok(c) => BufReader::new(c),
                Err(_) => return,
            };
            let mut rd = reader;
            // Простой вариант: читаем одну строку.
            if rd.read_line(&mut line).is_err() {
                return;
            }
            let resp = match serde_json::from_str::<Req>(&line) {
                Ok(req) => {
                    if let Some(set) = req.set {
                        if let Some(v) = set.language {
                            if let Ok(mut l) = st.lang.lock() {
                                *l = v;
                            }
                        }
                        if let Some(v) = set.model {
                            if let Ok(mut m) = st.model.lock() {
                                *m = v;
                            }
                        }
                        if let Some(v) = set.auto_paste {
                            if let Ok(mut a) = st.auto_paste.lock() {
                                *a = v;
                            }
                        }
                        if let Some(v) = set.prompt {
                            if let Ok(mut p) = st.prompt.lock() {
                                *p = v;
                            }
                        }
                    }
                    st.snapshot().to_string()
                }
                Err(e) => serde_json::to_string(&ErrResp {
                    ok: false,
                    error: format!("bad request: {e}"),
                })
                .unwrap(),
            };
            let _ = s.write_all(resp.as_bytes());
            let _ = s.write_all(b"\n");
        });
    }
    Ok(())
}
