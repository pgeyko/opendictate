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
    /// Язык распознавания: ru | en | auto
    #[arg(short, long, default_value = "ru")]
    language: String,

    /// Модель whisper: имя пресета (tiny|base|small|turbo|...) или путь к .bin
    #[arg(short, long, default_value = "base")]
    model: String,

    /// Beam size декодера (точность; 5 по умолчанию)
    #[arg(long, default_value = "5")]
    beam: u32,

    /// Автовставка Ctrl+V после распознавания (по умолчанию вкл)
    #[arg(long, default_value = "true", action = clap::ArgAction::Set)]
    auto_paste: bool,

    /// Initial prompt (подсказка-контекст; пусто = встроенный IT-пrompt)
    #[arg(long)]
    prompt: Option<String>,

    /// Включить отладочный режим с подробными логами
    #[arg(short, long, default_value = "false")]
    debug: bool,
}

/// Резолв спеки модели в (путь, опциональный URL для докачки).
fn resolve_model_spec(spec: &str) -> (PathBuf, Option<String>) {
    use opendictate_stt::{preset_url, resolve_model};
    let path = resolve_model(spec);
    if path.exists() {
        return (path, None);
    }
    // Имя пресета без файла -> качаем по URL пресета.
    if let Some(url) = preset_url(spec) {
        return (path, Some(url));
    }
    // Путь, которого нет, без URL — SttEngine выдаст понятную ошибку.
    (path, None)
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Включаем отладочный режим если флаг --debug
    if args.debug {
        opendictate_stt::debug::set_debug(true);
        eprintln!("[info] Отладочный режим включён");
    }

    if args.debug {
        eprintln!("=======================================================");
        eprintln!("   OpenDictate — Система голосовой диктовки для Linux   ");
        eprintln!("=======================================================");
    }

    let (model_path, model_url) = resolve_model_spec(&args.model);
    eprintln!("[init] Инициализация движка Whisper: {}", model_path.display());
    let mut stt = SttEngine::new_with_download(&model_path, model_url)?;
    eprintln!("[init] Движок Whisper готов!");
    let mut loaded_model = args.model.clone();

    let recorder = AudioRecorder::new();
    let _audio_stream = recorder.spawn_stream()?;
    eprintln!("[init] Захват звука с микрофона (PipeWire/parec) активен.");

    let injector = TextInjector::new();
    let mut paster = opendictate_input::AutoPaster::new();
    let auto_paste_mutex = Arc::new(std::sync::Mutex::new(args.auto_paste));
    if args.auto_paste {
        if paster.is_available() {
            eprintln!("[init] Автовставка Ctrl+V через uinput активна.");
        } else {
            eprintln!("[init] Автовставка недоступна (нет /dev/uinput) — только буфер обмена.");
        }
    }

    if !args.debug {
        eprintln!("\nГотово! Инструкция:");
        eprintln!("-> Нажмите и удерживайте клавишу [Pause] (или ScrollLock), чтобы говорить.");
        eprintln!("-> Отпустите клавишу, чтобы закончить фразу и скопировать текст.");
        eprintln!("(Текст копируется в системный буфер обмена: доступен по Ctrl+V)\n");
    } else {
        opendictate_stt::debug::debug_msg("Пропускаю вывод инструкции (режим отладки)");
    }

    let is_recording_bool = Arc::new(std::sync::Mutex::new(false));
    let lang_mutex = Arc::new(std::sync::Mutex::new(args.language.clone()));
    let model_mutex = Arc::new(std::sync::Mutex::new(args.model.clone()));

    // Запускаем системный трей (ksni)
    let tray_handle = opendictate_tray::spawn_tray(
        is_recording_bool.clone(),
        lang_mutex.clone(),
        model_mutex.clone(),
        auto_paste_mutex.clone(),
    )
    .ok();
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
            // Хот-релоад модели, если в трее выбрали другую.
            let wanted_model = model_mutex.lock().map(|m| m.clone()).unwrap_or_else(|_| "base".into());
            if wanted_model != loaded_model {
                eprintln!("[stt] Смена модели {loaded_model} -> {wanted_model}, перезагружаю...");
                let (mpath, murl) = resolve_model_spec(&wanted_model);
                match SttEngine::new_with_download(&mpath, murl) {
                    Ok(engine) => {
                        stt = engine;
                        loaded_model = wanted_model.clone();
                        eprintln!("[stt] Модель {wanted_model} готова");
                    }
                    Err(e) => {
                        eprintln!("[stt] Не удалось загрузить {wanted_model}: {e:#}, остаюсь на {loaded_model}");
                        if let Ok(mut m) = model_mutex.lock() {
                            *m = loaded_model.clone();
                        }
                    }
                }
            }
            let prompt = args.prompt.as_deref().unwrap_or(opendictate_stt::DEFAULT_PROMPT);
            let opts = opendictate_stt::TranscribeOpts {
                language: &current_lang,
                prompt,
                beam_size: args.beam,
                ..Default::default()
            };
            let start = Instant::now();
            match stt.transcribe(&audio_samples, &opts) {
                Ok(text) => {
                    let elapsed = start.elapsed().as_millis();
                    if text.is_empty() {
                        eprintln!("[stt] Речь не распознана.");
                    } else {
                        eprintln!("[stt] Распознано за {} мс: \"{}\"", elapsed, text);
                        if let Err(e) = injector.copy_to_clipboard(&text) {
                            eprintln!("[input] Ошибка копирования в буфер: {e}");
                            continue;
                        }
                        let paste_on = auto_paste_mutex
                            .lock()
                            .map(|v| *v)
                            .unwrap_or(false);
                        if paste_on {
                            match paster.paste() {
                                Ok(()) => eprintln!("[input] Вставлено через Ctrl+V"),
                                Err(e) => eprintln!("[input] Автовставка не удалась ({e:#}); текст в буфере, нажмите Ctrl+V"),
                            }
                        } else {
                            eprintln!("[input] Текст в буфере обмена (нажмите Ctrl+V для вставки)");
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
