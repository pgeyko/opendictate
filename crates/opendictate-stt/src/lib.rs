//! Модуль локального распознавания речи на базе whisper.cpp (GGML).
//!
//! Автономно собирает или находит бинарник `whisper-cli` без требования `libclang`.
//! Поддерживает автоматическую загрузку моделей GGML (по умолчанию ggml-base.bin).

use anyhow::{Context, Result};
use hound::{SampleFormat, WavSpec, WavWriter};
use std::fs::File;
use std::io::{copy, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const DEFAULT_MODEL_NAME: &str = "ggml-base.bin";
pub const DEFAULT_MODEL_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin";

pub struct SttEngine {
    model_path: PathBuf,
    cli_bin: PathBuf,
}

impl SttEngine {
    pub fn new<P: AsRef<Path>>(model_path: P) -> Result<Self> {
        let path = model_path.as_ref().to_path_buf();
        if !path.exists() {
            ensure_model_downloaded(&path, DEFAULT_MODEL_URL)?;
        }

        let cli_bin = ensure_whisper_cli()?;

        Ok(Self {
            model_path: path,
            cli_bin,
        })
    }

    /// Распознать 16 кГц моно PCM (сэмплы f32 в диапазоне -1.0..1.0)
    pub fn transcribe(&mut self, audio: &[f32], language: Option<&str>) -> Result<String> {
        if audio.is_empty() {
            return Ok(String::new());
        }

        // Сохраняем временный 16 кГц WAV файл
        let tmp_wav = std::env::temp_dir().join(format!("opendictate_rec_{}.wav", std::process::id()));
        write_wav(&tmp_wav, audio)?;

        let lang = language.unwrap_or("ru");

        // Вызываем оптимизированный whisper-cli
        let output = Command::new(&self.cli_bin)
            .args([
                "-m", self.model_path.to_str().unwrap(),
                "-f", tmp_wav.to_str().unwrap(),
                "-l", lang,
                "-nt", // без таймстемпов
                "-t", "4",
            ])
            .output()
            .context("Ошибка выполнения whisper-cli")?;

        let _ = std::fs::remove_file(&tmp_wav);

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("whisper-cli завершился с ошибкой: {}", err);
        }

        let text = String::from_utf8_lossy(&output.stdout);
        Ok(clean_transcription(&text))
    }
}

fn clean_transcription(raw: &str) -> String {
    let mut lines = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        // Отсекаем логи и галлюцинации Whisper на тишине («Редактор субтитров...»)
        if !trimmed.is_empty() 
            && !trimmed.starts_with('[') 
            && !trimmed.contains("Редактор субтитров")
            && !trimmed.contains("Корректор")
        {
            lines.push(trimmed);
        }
    }
    lines.join(" ")
}

fn write_wav(path: &Path, samples: &[f32]) -> Result<()> {
    let spec = WavSpec {
        channels: 1,
        sample_rate: 16000,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(path, spec)?;
    for &s in samples {
        let val = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        writer.write_sample(val)?;
    }
    writer.finalize()?;
    Ok(())
}

fn ensure_model_downloaded(dest: &Path, url: &str) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    eprintln!("[stt] Модель не найдена локально. Скачивание {}", url);
    let resp = reqwest::blocking::get(url)
        .context("Ошибка запроса к серверу моделей")?;

    if !resp.status().is_success() {
        anyhow::bail!("Сервер вернул ошибку при скачивании модели: {}", resp.status());
    }

    let mut out = File::create(dest)
        .context("Не удалось создать файл модели на диске")?;
    let mut reader = BufReader::new(resp);

    copy(&mut reader, &mut out)
        .context("Ошибка записи модели на диск")?;

    eprintln!("[stt] Модель успешно загружена в {}", dest.display());
    Ok(())
}

fn ensure_whisper_cli() -> Result<PathBuf> {
    // 1. Проверяем наличие в PATH или в ~/.local/bin
    if let Ok(output) = Command::new("which").arg("whisper-cli").output() {
        if output.status.success() {
            let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path_str.is_empty() {
                return Ok(PathBuf::from(path_str));
            }
        }
    }
    let local_bin = PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/bin/whisper-cli");
    if local_bin.is_file() {
        return Ok(local_bin);
    }

    // 2. Локальный кэш билда
    let cache_dir = PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cache/opendictate");
    let cached_bin = cache_dir.join("whisper-cli");
    if cached_bin.is_file() {
        return Ok(cached_bin);
    }

    // 3. Автоматическая сборка whisper-cli из официального репозитория
    std::fs::create_dir_all(&cache_dir)?;
    let src_dir = cache_dir.join("whisper.cpp");
    eprintln!("[stt] Сборка оптимизированного whisper-cli из исходников...");

    if !src_dir.exists() {
        let st = Command::new("git")
            .args(["clone", "--depth", "1", "https://github.com/ggerganov/whisper.cpp.git"])
            .arg(&src_dir)
            .status()
            .context("Не удалось склонировать whisper.cpp")?;
        if !st.success() {
            anyhow::bail!("git clone whisper.cpp завершился с ошибкой");
        }
    }

    let num_cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let st = Command::new("make")
        .arg(format!("-j{num_cpus}"))
        .current_dir(&src_dir)
        .status()
        .context("Не удалось собрать whisper.cpp через make")?;

    if !st.success() {
        anyhow::bail!("Сборка whisper-cli завершилась с ошибкой");
    }

    let built_bin = src_dir.join("build/bin/whisper-cli");
    if built_bin.is_file() {
        let _ = std::fs::copy(&built_bin, &cached_bin);
        return Ok(cached_bin);
    }

    anyhow::bail!("Файл whisper-cli не найден после сборки")
}

pub fn default_model_path() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".local/share")
        });
    base.join("opendictate/models").join(DEFAULT_MODEL_NAME)
}
