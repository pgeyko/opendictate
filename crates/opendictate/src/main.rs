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
use opendictate_stt::SttEngine;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

mod config;
mod control;

#[derive(Parser, Debug)]
#[command(name = "opendictate", about = "OpenDictate: voice typing for Linux")]
struct Args {
    /// Язык распознавания: ru | en | auto (иначе из конфига, иначе ru)
    #[arg(short, long)]
    language: Option<String>,

    /// Модель whisper: имя пресета или путь к .bin (иначе из конфига, иначе base)
    #[arg(short, long)]
    model: Option<String>,

    /// Beam size декодера (точность; 5 по умолчанию)
    #[arg(long, default_value = "5")]
    beam: u32,

    /// Автовставка Ctrl+V после распознавания (иначе из конфига, иначе вкл)
    #[arg(long, action = clap::ArgAction::Set)]
    auto_paste: Option<bool>,

    /// Initial prompt (подсказка-контекст; пусто = встроенный IT-prompt)
    #[arg(long)]
    prompt: Option<String>,

    /// Горячая клавиша: pause | f12 | scrolllock (иначе из конфига)
    #[arg(long)]
    hotkey: Option<String>,

    /// Режим тоггл: нажал — пишет, нажал ещё раз — вставил (иначе push-to-talk)
    #[arg(long, action = clap::ArgAction::SetTrue)]
    toggle: bool,

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

    let cfg = config::load();
    let language = args.language.clone().or(cfg.language).unwrap_or_else(|| "ru".into());
    let model_spec = args.model.clone().or(cfg.model).unwrap_or_else(|| "base".into());
    let auto_paste = args.auto_paste.or(cfg.auto_paste).unwrap_or(true);
    let cli_prompt = args.prompt.clone().or(cfg.prompt);
    let beam = args.beam;
    let hotkey = args
        .hotkey
        .as_deref()
        .and_then(config::Hotkey::parse)
        .or(cfg.hotkey)
        .unwrap_or_default();
    let hotkey_mode = if args.toggle {
        config::HotkeyMode::Toggle
    } else {
        cfg.hotkey_mode.unwrap_or_default()
    };
    let hotkey_name = hotkey.name().to_string();
    let hotkey_mode_name = match hotkey_mode {
        config::HotkeyMode::Toggle => "toggle".to_string(),
        config::HotkeyMode::Push => "push".to_string(),
    };
    eprintln!("[init] Хоткей: {hotkey:?} ({hotkey_mode:?}), язык: {language}, модель: {model_spec}");

    let (model_path, model_url) = resolve_model_spec(&model_spec);
    eprintln!("[init] Инициализация движка Whisper: {}", model_path.display());
    let mut stt = SttEngine::new_with_download(&model_path, model_url)?;
    eprintln!("[init] Движок Whisper готов!");

    let recorder = AudioRecorder::new();
    let _audio_stream = recorder.spawn_stream()?;
    eprintln!("[init] Захват звука с микрофона (PipeWire/parec) активен.");

    let injector = TextInjector::new();
    let mut paster = opendictate_input::AutoPaster::new();
    let auto_paste_mutex = Arc::new(std::sync::Mutex::new(auto_paste));
    if auto_paste {
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
    let lang_mutex = Arc::new(std::sync::Mutex::new(language.clone()));
    let model_mutex = Arc::new(std::sync::Mutex::new(model_spec.clone()));
    let loaded_mutex = Arc::new(std::sync::Mutex::new(model_spec.clone()));
    let prompt_mutex = Arc::new(std::sync::Mutex::new(cli_prompt.clone().unwrap_or_default()));
    let last_mutex = Arc::new(std::sync::Mutex::new((String::new(), 0u128)));
    let hotkey_mutex = Arc::new(std::sync::Mutex::new(hotkey_name.clone()));
    let mode_mutex = Arc::new(std::sync::Mutex::new(hotkey_mode_name.clone()));

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

    // Control-сокет для GUI/CLI (статус + set без рестарта).
    let shared = control::Shared {
        lang: lang_mutex.clone(),
        model: model_mutex.clone(),
        loaded_model: loaded_mutex.clone(),
        auto_paste: auto_paste_mutex.clone(),
        prompt: prompt_mutex.clone(),
        recording: is_recording_bool.clone(),
        last: last_mutex.clone(),
        hotkey: hotkey_mutex.clone(),
        hotkey_mode: mode_mutex.clone(),
    };
    std::thread::spawn(move || {
        if let Err(e) = control::serve(shared) {
            eprintln!("[ctl] control socket error: {e:#}");
        }
    });

    let is_recording = Arc::new(AtomicBool::new(false));

    // Находим клавиатурные устройства в /dev/input/event*
    let devices = find_keyboards()?;
    if devices.is_empty() {
        eprintln!("[warn] Клавиатурные устройства ввода не найдены. Проверьте права пользователя в группе input.");
    }

    let (tx, rx) = std::sync::mpsc::channel::<(Key, bool)>();

    // Слушатели событий клавиатуры на evdev потоках.
    // Фильтр по хоткею читается из shared (меняется на лету из GUI/трея).
    for mut dev in devices {
        let sender = tx.clone();
        let hotkey_mutex = hotkey_mutex.clone();
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
                            let want = hotkey_mutex
                                .lock()
                                .map(|n| {
                                    config::Hotkey::parse(&n).map(|h| h.evdev_key()) == Some(key)
                                })
                                .unwrap_or(false);
                            if want {
                                let _ = sender.send((key, pressed));
                            }
                        }
                    }
                }
                Err(_) => break,
            }
        });
    }

    // Главный цикл обработки голосового ввода.
    // Push-to-talk: зажал — пишет, отпустил — вставил.
    // Toggle: нажал — пишет, нажал ещё раз — вставил.
    // Режим читается на каждую фразу (переключается на лету).
    while let Ok((key, pressed)) = rx.recv() {
        let toggle_mode = mode_mutex
            .lock()
            .map(|m| m.as_str() == "toggle")
            .unwrap_or(false);
        if toggle_mode {
            if !pressed {
                continue; // в тоггле работаем только по нажатиям
            }
            if !is_recording.load(Ordering::Relaxed) {
                start_recording(
                    &is_recording,
                    &is_recording_bool,
                    &tray_handle,
                    &recorder,
                    key,
                );
            } else {
                stop_and_transcribe(
                    &is_recording,
                    &is_recording_bool,
                    &tray_handle,
                    &recorder,
                    &mut stt,
                    &injector,
                    &mut paster,
                    &lang_mutex,
                    &model_mutex,
                    &loaded_mutex,
                    &prompt_mutex,
                    &cli_prompt,
                    &auto_paste_mutex,
                    &last_mutex,
                    beam,
                );
            }
            continue;
        }
        if pressed {
            if !is_recording.load(Ordering::Relaxed) {
                start_recording(
                    &is_recording,
                    &is_recording_bool,
                    &tray_handle,
                    &recorder,
                    key,
                );
            }
        } else if is_recording.load(Ordering::Relaxed) {
            stop_and_transcribe(
                &is_recording,
                &is_recording_bool,
                &tray_handle,
                &recorder,
                &mut stt,
                &injector,
                &mut paster,
                &lang_mutex,
                &model_mutex,
                &loaded_mutex,
                &prompt_mutex,
                &cli_prompt,
                &auto_paste_mutex,
                &last_mutex,
                beam,
            );
        }
    }

    Ok(())
}

/// Начать запись: флаги, трей, звук, буфер.
#[allow(clippy::too_many_arguments)]
fn start_recording(
    is_recording: &Arc<AtomicBool>,
    is_recording_bool: &Arc<std::sync::Mutex<bool>>,
    tray_handle: &Option<ksni::blocking::Handle<opendictate_tray::DictateTray>>,
    recorder: &AudioRecorder,
    key: Key,
) {
    is_recording.store(true, Ordering::Relaxed);
    if let Ok(mut r) = is_recording_bool.lock() {
        *r = true;
    }
    if let Some(t) = tray_handle {
        t.update(|_| {});
    }
    eprintln!("\n[mic] ● Слушаю... (клавиша {:?})", key);
    opendictate_core::sound::play_start_sound();
    recorder.start();
}

/// Остановить запись, распознать, скопировать и (опционально) вставить.
#[allow(clippy::too_many_arguments)]
fn stop_and_transcribe(
    is_recording: &Arc<AtomicBool>,
    is_recording_bool: &Arc<std::sync::Mutex<bool>>,
    tray_handle: &Option<ksni::blocking::Handle<opendictate_tray::DictateTray>>,
    recorder: &AudioRecorder,
    stt: &mut SttEngine,
    injector: &TextInjector,
    paster: &mut opendictate_input::AutoPaster,
    lang_mutex: &Arc<std::sync::Mutex<String>>,
    model_mutex: &Arc<std::sync::Mutex<String>>,
    loaded_mutex: &Arc<std::sync::Mutex<String>>,
    prompt_mutex: &Arc<std::sync::Mutex<String>>,
    cli_prompt: &Option<String>,
    auto_paste_mutex: &Arc<std::sync::Mutex<bool>>,
    last_mutex: &Arc<std::sync::Mutex<(String, u128)>>,
    beam: u32,
) {
    is_recording.store(false, Ordering::Relaxed);
    if let Ok(mut r) = is_recording_bool.lock() {
        *r = false;
    }
    if let Some(t) = tray_handle {
        t.update(|_| {});
    }
    opendictate_core::sound::play_stop_sound();
    eprintln!("[mic] Запись завершена. Распознавание нейросетью...");
    let audio_samples = recorder.stop();

    if audio_samples.len() < 16000 / 2 {
        eprintln!("[mic] Фраза слишком короткая (<0.5с), пропуск.");
        return;
    }

    let current_lang = lang_mutex.lock().map(|l| l.clone()).unwrap_or_else(|_| "ru".into());
    // Хот-релоад модели, если в трее/GUI выбрали другую.
    let wanted_model = model_mutex.lock().map(|m| m.clone()).unwrap_or_else(|_| "base".into());
    let loaded_now = loaded_mutex.lock().map(|m| m.clone()).unwrap_or_else(|_| "base".into());
    if wanted_model != loaded_now {
        eprintln!("[stt] Смена модели {loaded_now} -> {wanted_model}, перезагружаю...");
        let (mpath, murl) = resolve_model_spec(&wanted_model);
        match SttEngine::new_with_download(&mpath, murl) {
            Ok(engine) => {
                *stt = engine;
                if let Ok(mut m) = loaded_mutex.lock() {
                    *m = wanted_model.clone();
                }
                eprintln!("[stt] Модель {wanted_model} готова");
            }
            Err(e) => {
                eprintln!("[stt] Не удалось загрузить {wanted_model}: {e:#}, остаюсь на {loaded_now}");
                if let Ok(mut m) = model_mutex.lock() {
                    *m = loaded_now.clone();
                }
            }
        }
    }
    let prompt_cfg = prompt_mutex.lock().map(|p| p.clone()).unwrap_or_default();
    let prompt_owned: String = if prompt_cfg.is_empty() {
        cli_prompt
            .clone()
            .unwrap_or_else(|| opendictate_stt::DEFAULT_PROMPT.to_string())
    } else {
        prompt_cfg
    };
    let opts = opendictate_stt::TranscribeOpts {
        language: &current_lang,
        prompt: &prompt_owned,
        beam_size: beam,
        ..Default::default()
    };
    let start = Instant::now();
    match stt.transcribe(&audio_samples, &opts) {
        Ok(text) => {
            let elapsed = start.elapsed().as_millis();
            if let Ok(mut l) = last_mutex.lock() {
                *l = (text.clone(), elapsed);
            }
            if text.is_empty() {
                eprintln!("[stt] Речь не распознана.");
            } else {
                eprintln!("[stt] Распознано за {} мс: \"{}\"", elapsed, text);
                if let Err(e) = injector.copy_to_clipboard(&text) {
                    eprintln!("[input] Ошибка копирования в буфер: {e}");
                    return;
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
