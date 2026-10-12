//! Модуль аудио-захвата и буферизации с микрофона.
//!
//! Использует системный `parec` (PipeWire / PulseAudio) для прямой записи
//! моно-потока 16 кГц S16LE. Это гарантирует 100% совместимость со всеми микрофонами
//! (включая OpenMic) без необходимости установки dev-пакетов alsa/jack.

use anyhow::{Context, Result};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

pub const TARGET_SAMPLE_RATE: u32 = 16000;

#[derive(Clone)]
pub struct AudioRecorder {
    buffer: Arc<Mutex<Vec<f32>>>,
    recording: Arc<Mutex<bool>>,
}

pub struct ActiveStream {
    child: Child,
    alive: Arc<Mutex<bool>>,
}

impl Drop for ActiveStream {
    fn drop(&mut self) {
        if let Ok(mut a) = self.alive.lock() {
            *a = false;
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl AudioRecorder {
    pub fn new() -> Self {
        Self {
            buffer: Arc::new(Mutex::new(Vec::with_capacity(TARGET_SAMPLE_RATE as usize * 10))),
            recording: Arc::new(Mutex::new(false)),
        }
    }

    /// Начать запись в буфер
    pub fn start(&self) {
        if let Ok(mut rec) = self.recording.lock() {
            *rec = true;
        }
        if let Ok(mut buf) = self.buffer.lock() {
            buf.clear();
        }
    }

    /// Остановить запись и вернуть накопленные сэмплы
    pub fn stop(&self) -> Vec<f32> {
        if let Ok(mut rec) = self.recording.lock() {
            *rec = false;
        }
        if let Ok(mut buf) = self.buffer.lock() {
            let res = buf.clone();
            buf.clear();
            res
        } else {
            Vec::new()
        }
    }

    pub fn is_recording(&self) -> bool {
        self.recording.lock().map(|r| *r).unwrap_or(false)
    }

    /// Запустить фоновый процесс захвата звука (PipeWire/PulseAudio parec)
    pub fn spawn_stream(&self) -> Result<ActiveStream> {
        let mut child = Command::new("parec")
            .args([
                "--format=s16le",
                &format!("--rate={TARGET_SAMPLE_RATE}"),
                "--channels=1",
                "--raw",
                "--latency-msec=50",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("Не удалось запустить parec. Убедитесь, что PipeWire запущен.")?;

        let mut stdout = child.stdout.take().context("Нет доступа к stdout parec")?;
        let buf_clone = self.buffer.clone();
        let rec_clone = self.recording.clone();
        let alive = Arc::new(Mutex::new(true));
        let alive_thread = alive.clone();

        thread::spawn(move || {
            let mut raw = [0u8; 1024];
            while alive_thread.lock().map(|a| *a).unwrap_or(false) {
                match stdout.read(&mut raw) {
                    Ok(0) => break,
                    Ok(n) => {
                        if !rec_clone.lock().map(|r| *r).unwrap_or(false) {
                            continue;
                        }
                        // Преобразуем S16LE в нормализованный f32 (-1.0..1.0)
                        let mut f32_samples = Vec::with_capacity(n / 2);
                        for chunk in raw[..n].chunks_exact(2) {
                            let sample = i16::from_le_bytes([chunk[0], chunk[1]]);
                            f32_samples.push(sample as f32 / 32768.0);
                        }

                        if let Ok(mut b) = buf_clone.lock() {
                            b.extend_from_slice(&f32_samples);
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(ActiveStream { child, alive })
    }
}
