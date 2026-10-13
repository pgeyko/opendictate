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

pub mod debug;

/// Дефолтный bilingual IT-prompt: держит русские окончания и латиницу терминов.
pub const DEFAULT_PROMPT: &str = "Это разговор о программировании на русском с английскими терминами. Термины: whisper, лог, буфер обмена, PipeWire, демон, токен, коммит, демон, микрофон, шумодав.";

const HF_BASE: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

/// Пресет: (имя, файл, вес МБ, описание).
pub const MODEL_PRESETS: &[(&str, &str, u64, &str)] = &[
    ("tiny", "ggml-tiny.bin", 75, "минимум, команды"),
    ("base", "ggml-base.bin", 142, "по умолчанию"),
    ("base-q5_1", "ggml-base-q5_1.bin", 100, "легче base"),
    ("small", "ggml-small.bin", 466, "качественный русский"),
    ("small-q5_1", "ggml-small-q5_1.bin", 350, "качество/вес оптимум"),
    ("medium", "ggml-medium.bin", 1500, "студийная точность"),
    ("turbo", "ggml-large-v3-turbo.bin", 809, "максимум без боли"),
    ("turbo-q5_0", "ggml-large-v3-turbo-q5_0.bin", 600, "максимум, легче"),
    ("turbo-q8_0", "ggml-large-v3-turbo-q8_0.bin", 850, "максимум точности"),
];

/// Резолв имени пресета или пути в (файл, нужно_ли_качать).
/// Возвращает путь к файлу модели.
pub fn resolve_model(spec: &str) -> PathBuf {
    if let Some((_, file, _, _)) = MODEL_PRESETS.iter().find(|(name, _, _, _)| *name == spec) {
        return default_models_dir().join(file);
    }
    PathBuf::from(spec)
}

/// URL для скачивания пресета, если spec — имя пресета.
pub fn preset_url(spec: &str) -> Option<String> {
    MODEL_PRESETS
        .iter()
        .find(|(name, _, _, _)| *name == spec)
        .map(|(_, file, _, _)| format!("{HF_BASE}/{file}"))
}

pub fn default_models_dir() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".local/share")
        });
    base.join("opendictate/models")
}

pub struct SttEngine {
    model_path: PathBuf,
    cli_bin: PathBuf,
}

/// Параметры одного распознавания.
#[derive(Debug, Clone)]
pub struct TranscribeOpts<'a> {
    pub language: &'a str,
    pub prompt: &'a str,
    pub beam_size: u32,
    pub temperature: f32,
    pub temp_inc: f32,
    pub no_speech_thold: f32,
    pub threads: u32,
}

impl<'a> Default for TranscribeOpts<'a> {
    fn default() -> Self {
        Self {
            language: "ru",
            prompt: DEFAULT_PROMPT,
            beam_size: 5,
            temperature: 0.0,
            temp_inc: 0.2,
            no_speech_thold: 0.6,
            threads: 4,
        }
    }
}

impl SttEngine {
    pub fn new<P: AsRef<Path>>(model_path: P) -> Result<Self> {
        Self::new_with_download(model_path, None)
    }

    /// model_spec: путь к файлу или имя пресета (тогда url тоже из пресета).
    pub fn new_with_download<P: AsRef<Path>>(model_path: P, preset_url: Option<String>) -> Result<Self> {
        let path = model_path.as_ref().to_path_buf();
        debug::debug_msg(&format!("Инициализация STT движка, модель: {}", path.display()));
        if !path.exists() {
            let url = preset_url.unwrap_or_else(|| DEFAULT_MODEL_URL.to_string());
            debug::debug_msg("Модель не найдена локально, скачиваю...");
            ensure_model_downloaded(&path, &url)?;
            debug::debug_msg("Модель успешно скачана");
        }

        let cli_bin = ensure_whisper_cli()?;
        debug::debug_msg(&format!("whisper-cli найден: {}", cli_bin.display()));
        debug::debug_msg(&format!("Размер модели: {}", path.metadata().map(|m| m.len()).unwrap_or(0)));

        Ok(Self {
            model_path: path,
            cli_bin,
        })
    }

    /// Распознать 16 кГц моно PCM (сэмплы f32 в диапазоне -1.0..1.0).
    /// Вход предобрабатывается: обрезка тишины (VAD) + нормализация пика.
    pub fn transcribe(&mut self, audio: &[f32], opts: &TranscribeOpts) -> Result<String> {
        let processed = preprocess(audio);
        if processed.is_empty() {
            debug::debug_msg("После VAD остался пустой буфер, пропускаю распознавание");
            return Ok(String::new());
        }
        debug::debug_msg(&format!(
            "Препроцессинг: {} -> {} сэмплов",
            audio.len(),
            processed.len()
        ));

        // Сохраняем временный 16 кГц WAV файл
        let tmp_wav = std::env::temp_dir().join(format!("opendictate_rec_{}.wav", std::process::id()));
        debug::debug_msg(&format!(
            "Создаю WAV файл: {} ({} сэмплов, {} сек)",
            tmp_wav.display(),
            processed.len(),
            processed.len() / 16000_usize as usize
        ));
        write_wav(&tmp_wav, &processed)?;

        debug::debug_msg(&format!(
            "Запуск whisper-cli: модель={}, wav={}, язык={}, beam={}, prompt={} симв.",
            self.model_path.display(),
            tmp_wav.display(),
            opts.language,
            opts.beam_size,
            opts.prompt.chars().count(),
        ));

        // Вызываем оптимизированный whisper-cli
        let start = std::time::Instant::now();
        let mut cmd = Command::new(&self.cli_bin);
        cmd.args([
            "-m", self.model_path.to_str().unwrap(),
            "-f", tmp_wav.to_str().unwrap(),
            "-l", opts.language,
            "-nt", // без таймстемпов
            "-t", &opts.threads.to_string(),
            "-bs", &opts.beam_size.to_string(),
            "-tp", &opts.temperature.to_string(),
            "-tpi", &opts.temp_inc.to_string(),
            "-nth", &opts.no_speech_thold.to_string(),
        ]);
        if !opts.prompt.is_empty() {
            cmd.args(["--prompt", opts.prompt]);
        }
        let output = cmd.output().context("Ошибка выполнения whisper-cli")?;
        let elapsed = start.elapsed();
        debug::debug_msg(&format!("whisper-cli завершился за {:?}", elapsed));

        let _ = std::fs::remove_file(&tmp_wav);
        debug::debug_msg("Временный WAV файл удалён");

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            debug::debug_msg(&format!("Ошибка whisper-cli: {}", err));
            anyhow::bail!("whisper-cli завершился с ошибкой: {}", err);
        }

        let text = String::from_utf8_lossy(&output.stdout);
        debug::debug_msg(&format!("Сырой вывод whisper-cli:\n{}", text));
        let cleaned = clean_transcription(&text);
        debug::debug_msg(&format!("Очищенный текст: \"{}\"", cleaned));

        Ok(cleaned)
    }

    pub fn model_path(&self) -> &Path {
        &self.model_path
    }
}

/// VAD-обрезка тишины по краям + нормализация пика к 0.9.
/// Убирает ведущую/хвостовую тишину (провоцирует галлюцинации) и выравнивает
/// тихий микрофон до уровня, на котором модель уверенно работает.
pub fn preprocess(audio: &[f32]) -> Vec<f32> {
    if audio.is_empty() {
        return Vec::new();
    }
    // RMS по фреймам 20 мс, порог тишины.
    const FRAME: usize = 320; // 20 мс при 16 кГц
    const THRESH: f32 = 0.015;
    let is_speech = |f: &[f32]| {
        let sum: f32 = f.iter().map(|s| s * s).sum();
        (sum / f.len() as f32).sqrt() > THRESH
    };
    let n_frames = audio.len() / FRAME;
    let mut first = n_frames;
    let mut last = 0usize;
    for (i, fr) in audio.chunks_exact(FRAME).enumerate() {
        if is_speech(fr) {
            if i < first {
                first = i;
            }
            last = i;
        }
    }
    if first > last {
        return Vec::new(); // одна тишина
    }
    // Небольшой запас по 100 мс с каждой стороны, чтобы не резать согласные.
    let pad = 1600usize;
    let start = first.saturating_mul(FRAME).saturating_sub(pad);
    let end = ((last + 1) * FRAME + pad).min(audio.len());
    let mut out = audio[start..end].to_vec();
    // Нормализация пика к 0.9.
    let peak = out.iter().map(|s| s.abs()).fold(0f32, f32::max);
    if peak > 1e-4 {
        let g = (0.9 / peak).min(8.0); // не разгоняем шум больше чем в 8 раз
        for s in out.iter_mut() {
            *s = (*s * g).clamp(-1.0, 1.0);
        }
    }
    out
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
    default_models_dir().join(DEFAULT_MODEL_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(len: usize, amp: f32) -> Vec<f32> {
        (0..len)
            .map(|i| amp * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 16000.0).sin())
            .collect()
    }

    #[test]
    fn silence_trims_to_empty() {
        assert!(preprocess(&vec![0.0; 16000]).is_empty());
        assert!(preprocess(&[]).is_empty());
    }

    #[test]
    fn trims_edges_and_normalizes_peak() {
        // 0.5 с тишины + 1 с тона 0.2 + 0.5 с тишины
        let mut audio = vec![0.0; 8000];
        audio.extend(sine(16000, 0.2));
        audio.extend(vec![0.0; 8000]);
        let out = preprocess(&audio);
        // тишина по краям срезана (с запасом 100 мс): явно короче входа
        assert!(out.len() < audio.len() - 8000, "len={}", out.len());
        // пик подтянут к 0.9
        let peak = out.iter().map(|s| s.abs()).fold(0f32, f32::max);
        assert!((peak - 0.9).abs() < 0.05, "peak={peak}");
    }

    #[test]
    fn loud_signal_not_clipped_beyond_one() {
        let out = preprocess(&sine(16000, 0.99));
        assert!(out.iter().all(|s| s.abs() <= 1.0));
    }

    #[test]
    fn presets_resolve() {
        assert!(resolve_model("small").ends_with("ggml-small.bin"));
        assert_eq!(resolve_model("/tmp/x.bin"), PathBuf::from("/tmp/x.bin"));
        assert!(preset_url("turbo").unwrap().ends_with("ggml-large-v3-turbo.bin"));
        assert!(preset_url("/tmp/x.bin").is_none());
    }
}
