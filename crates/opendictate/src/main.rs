//! Главный исполняемый бинарник OpenDictate.
//!
//! Режим Push-to-Talk / Хоткей через Linux evdev (группа `input`):
//! Зажимаем или нажимаем клавишу `Pause` -> говорим в микрофон ->
//! отпускаем -> нейросеть Whisper локально расшифровывает голос ->
//! текст копируется в буфер обмена `wl-copy`.

use anyhow::{Context, Result};
use clap::Parser;
use evdev::{Device, Key};
use opendictate_core::AudioRecorder;
use opendictate_input::TextInjector;
use opendictate_stt::{default_model_path, SttEngine};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[derive(Parser, Debug)]
#[command(name = "opendictate", about = "OpenDictate: voice typing for Linux")]
struct Args {
    /// Язык распознавания (по умолчанию ru)
    #[arg(short, long, default_value = "ru")]
    language: String,

    /// Путь к весам модели whisper (.bin)
    #[arg(short, long)]
    model: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    eprintln!("=======================================================");
    eprintln!("   OpenDictate — Система голосовой диктовки для Linux   ");
    eprintln!("=======================================================");

    let model_path = args.model.unwrap_or_else(default_model_path);
    eprintln!("[init] Инициализация движка Whisper: {}", model_path.display());
    let mut stt = SttEngine::new(&model_path)?;
    eprintln!("[init] Движок Whisper готов!");

    let recorder = AudioRecorder::new();
    let _audio_stream = recorder.spawn_stream()?;
    eprintln!("[init] Захват звука с микрофона (PipeWire/parec) активен.");

    let injector = TextInjector::new();

    eprintln!("\nГотово! Инструкция:");
    eprintln!("-> Нажмите и удерживайте клавишу [Pause] (или ScrollLock), чтобы говорить.");
    eprintln!("-> Отпустите клавишу, чтобы закончить фразу и скопировать текст.");
    eprintln!("(Текст копируется в системный буфер обмена: доступен по Ctrl+V)\n");

    let is_recording_bool = Arc::new(std::sync::Mutex::new(false));
    let lang_mutex = Arc::new(std::sync::Mutex::new(args.language.clone()));

    // Запускаем системный трей (ksni)
    let tray_handle = opendictate_tray::spawn_tray(is_recording_bool.clone(), lang_mutex.clone()).ok();
    if tray_handle.is_some() {
        eprintln!("[init] Иконка в системном трее активна.");
    }

    let is_recording = Arc::new(AtomicBool::new(false));

    // Находим клавиатурные устройства в /dev/input/event*
    let devices = find_keyboards()?;
    if devices.is_empty() {
        eprintln!("[warn] Клавиатурные устройства ввода не найдены. Проверьте права пользователя в группе input.");
    }

    let (tx, rx) = std::sync::mpsc::channel::<(Key, bool)>();

    // Слушатели событий клавиатуры на evdev потоках
    for mut dev in devices {
        let sender = tx.clone();
        std::thread::spawn(move || loop {
            match dev.fetch_events() {
                Ok(events) => {
                    for ev in events {
                        if let evdev::InputEventKind::Key(key) = ev.kind() {
                            let pressed = match ev.value() {
                                1 => true,
                                0 => false,
                                _ => continue,
                            };
                            if matches!(key, Key::KEY_PAUSE | Key::KEY_SCROLLLOCK | Key::KEY_F12) {
                                let _ = sender.send((key, pressed));
                            }
                        }
                    }
                }
                Err(_) => break,
            }
        });
    }

    // Главный цикл обработки голосового ввода
    while let Ok((key, pressed)) = rx.recv() {
        if pressed {
            if !is_recording.load(Ordering::Relaxed) {
                is_recording.store(true, Ordering::Relaxed);
                if let Ok(mut r) = is_recording_bool.lock() {
                    *r = true;
                }
                if let Some(t) = &tray_handle {
                    t.update(|_| {});
                }
                eprintln!("\n[mic] ● Слушаю... (клавиша {:?} зажата)", key);
                opendictate_core::sound::play_start_sound();
                recorder.start();
            }
        } else if is_recording.load(Ordering::Relaxed) {
            is_recording.store(false, Ordering::Relaxed);
            if let Ok(mut r) = is_recording_bool.lock() {
                *r = false;
            }
            if let Some(t) = &tray_handle {
                t.update(|_| {});
            }
            opendictate_core::sound::play_stop_sound();
            eprintln!("[mic] Запись завершена. Распознавание нейросетью...");
            let audio_samples = recorder.stop();

            if audio_samples.len() < 16000 / 2 {
                eprintln!("[mic] Фраза слишком короткая (<0.5с), пропуск.");
                continue;
            }

            let current_lang = lang_mutex.lock().map(|l| l.clone()).unwrap_or_else(|_| "ru".into());
            let start = Instant::now();
            match stt.transcribe(&audio_samples, Some(&current_lang)) {
                Ok(text) => {
                    let elapsed = start.elapsed().as_millis();
                    if text.is_empty() {
                        eprintln!("[stt] Речь не распознана.");
                    } else {
                        eprintln!("[stt] Распознано за {} мс: \"{}\"", elapsed, text);
                        if let Err(e) = injector.copy_to_clipboard(&text) {
                            eprintln!("[input] Ошибка копирования в буфер: {e}");
                        } else {
                            eprintln!("[input] Текст скопирован в буфер обмена (нажмите Ctrl+V для вставки)");
                        }
                    }
                }
                Err(e) => eprintln!("[stt] Ошибка распознавания: {e}"),
            }
        }
    }

    Ok(())
}

fn find_keyboards() -> Result<Vec<Device>> {
    let mut keyboards = Vec::new();
    let entries = fs::read_dir("/dev/input").context("Не удалось прочитать /dev/input")?;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.to_string_lossy().contains("event") {
            if let Ok(dev) = Device::open(&path) {
                if let Some(keys) = dev.supported_keys() {
                    if keys.contains(Key::KEY_SPACE) || keys.contains(Key::KEY_ENTER) {
                        keyboards.push(dev);
                    }
                }
            }
        }
    }

    Ok(keyboards)
}
