//! OpenDictate GTK4 GUI: статус, модель, язык, ввод.
//!
//! Единая точка входа: поднимает `opendictated`/`opendictate`, если сокет молчит,
//! гасит своего демона при выходе (PR_SET_PDEATHSIG). Чужой демон не трогает.

use adw::prelude::*;
use gtk::{gdk, glib};
use serde::Deserialize;
use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::rc::Rc;
use std::time::Duration;

const MODELS: &[(&str, &str)] = &[
    ("tiny", "минимум, команды (~75 МБ)"),
    ("base", "по умолчанию (~142 МБ)"),
    ("base-q5_1", "легче base (~100 МБ)"),
    ("small", "качественный русский (~466 МБ)"),
    ("small-q5_1", "качество/вес оптимум (~350 МБ)"),
    ("medium", "студийная точность (~1.5 ГБ)"),
    ("turbo", "максимум без боли (~809 МБ)"),
    ("turbo-q5_0", "максимум, легче (~600 МБ)"),
    ("turbo-q8_0", "максимум точности (~850 МБ)"),
];

const LANGS: &[(&str, &str)] = &[("ru", "Русский"), ("en", "English"), ("auto", "Авто")];

#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
struct State {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    language: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    loaded_model: String,
    #[serde(default)]
    auto_paste: bool,
    #[serde(default)]
    recording: bool,
    #[serde(default)]
    last_text: String,
    #[serde(default)]
    last_ms: u128,
}

fn sock_path() -> PathBuf {
    if let Ok(rt) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(rt).join("opendictate.sock");
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "def".into());
    PathBuf::from(format!("/tmp/opendictate-{user}.sock"))
}

fn send(req: &str) -> Option<State> {
    let mut s = UnixStream::connect(sock_path()).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    s.set_write_timeout(Some(Duration::from_secs(2))).ok()?;
    writeln!(s, "{req}").ok()?;
    let line = BufReader::new(s).lines().next()?.ok()?;
    serde_json::from_str(&line).ok()
}

// --- Супервизор демона ---

struct DaemonGuard {
    child: Option<Child>,
}

fn find_daemon() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let c = dir.join("opendictate");
            if c.is_file() {
                return c;
            }
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let c = PathBuf::from(&home).join(".local/bin/opendictate");
        if c.is_file() {
            return c;
        }
    }
    let c = PathBuf::from("target/debug/opendictate");
    if c.is_file() {
        return c;
    }
    PathBuf::from("opendictate")
}

impl DaemonGuard {
    fn start_if_needed(&mut self) -> bool {
        if send("{\"status\":true}").is_some() {
            return true;
        }
        let bin = find_daemon();
        let mut cmd = Command::new(&bin);
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        use std::os::unix::process::CommandExt as _;
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong);
                Ok(())
            });
        }
        match cmd.spawn() {
            Ok(child) => {
                self.child = Some(child);
                for _ in 0..50 {
                    std::thread::sleep(Duration::from_millis(100));
                    if send("{\"status\":true}").is_some() {
                        return true;
                    }
                }
                false
            }
            Err(_) => false,
        }
    }

    fn stop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn build_ui(app: &adw::Application, guard: Rc<RefCell<DaemonGuard>>) {
    guard.borrow_mut().start_if_needed();

    // --- Статус ---
    let dot = gtk::Label::new(Some("●"));
    let daemon_state = gtk::Label::builder().halign(gtk::Align::Start).hexpand(true).build();
    let last_lbl = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .wrap(true)
        .build();
    last_lbl.add_css_class("monospace");
    let start_btn = gtk::Button::with_label("Запустить демона");
    start_btn.set_valign(gtk::Align::Center);
    {
        let guard = guard.clone();
        start_btn.connect_clicked(move |_| {
            guard.borrow_mut().start_if_needed();
        });
    }
    let status_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    status_row.append(&dot);
    status_row.append(&daemon_state);
    status_row.append(&start_btn);

    let card_status = adw::PreferencesGroup::new();
    card_status.set_title("Состояние");
    let status_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    status_box.append(&status_row);
    status_box.append(&last_lbl);
    card_status.add(&status_box);

    // --- Модель ---
    let card_model = adw::PreferencesGroup::new();
    card_model.set_title("Модель");
    let model_row = adw::ComboRow::new();
    model_row.set_title("Распознавание");
    let model_names: Vec<String> = MODELS.iter().map(|(n, d)| format!("{n} — {d}")).collect();
    let model_list = gtk::StringList::new(
        &model_names.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
    );
    model_row.set_model(Some(&model_list));
    let model_note = gtk::Label::new(None);
    model_note.add_css_class("dim-label");
    model_note.set_halign(gtk::Align::Start);
    card_model.add(&model_row);
    card_model.add(&model_note);

    // --- Язык и prompt ---
    let card_lang = adw::PreferencesGroup::new();
    card_lang.set_title("Язык и контекст");
    let lang_row = adw::ComboRow::new();
    lang_row.set_title("Язык");
    let lang_names: Vec<&str> = LANGS.iter().map(|(_, d)| *d).collect();
    lang_row.set_model(Some(&gtk::StringList::new(&lang_names)));
    let prompt_row = adw::ActionRow::builder()
        .title("Контекст (prompt)")
        .subtitle("Пусто = встроенный IT-словарь")
        .build();
    let prompt_entry = gtk::Entry::builder()
        .placeholder_text("Свои термины через запятую…")
        .hexpand(true)
        .build();
    let prompt_apply = gtk::Button::with_label("OK");
    let prompt_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    prompt_box.append(&prompt_entry);
    prompt_box.append(&prompt_apply);
    prompt_row.add_suffix(&prompt_box);
    card_lang.add(&lang_row);
    card_lang.add(&prompt_row);

    // --- Ввод ---
    let card_input = adw::PreferencesGroup::new();
    card_input.set_title("Ввод");
    let paste_row = adw::ActionRow::builder()
        .title("Автовставка Ctrl+V")
        .subtitle("Печатать текст сразу после распознавания")
        .build();
    let paste = gtk::Switch::new();
    paste.set_valign(gtk::Align::Center);
    paste_row.add_suffix(&paste);
    let hotkey_row = adw::ActionRow::builder()
        .title("Горячая клавиша")
        .subtitle("Pause / ScrollLock / F12 (удерживать и говорить)")
        .build();
    card_input.add(&paste_row);
    card_input.add(&hotkey_row);

    // Начальное состояние (без эха: сначала выставить, потом подписать).
    if let Some(st) = send("{\"status\":true}") {
        if let Some(i) = MODELS.iter().position(|(n, _)| *n == st.model) {
            model_row.set_selected(i as u32);
        }
        if let Some(i) = LANGS.iter().position(|(n, _)| *n == st.language) {
            lang_row.set_selected(i as u32);
        }
        paste.set_active(st.auto_paste);
    }

    model_row.connect_selected_notify({
        move |row| {
            let i = row.selected() as usize;
            if let Some((name, _)) = MODELS.get(i) {
                send(&format!("{{\"set\":{{\"model\":\"{name}\"}}}}"));
            }
        }
    });
    lang_row.connect_selected_notify(move |row| {
        let i = row.selected() as usize;
        if let Some((name, _)) = LANGS.get(i) {
            send(&format!("{{\"set\":{{\"language\":\"{name}\"}}}}"));
        }
    });
    prompt_apply.connect_clicked({
        let prompt_entry = prompt_entry.clone();
        move |_| {
            let text = prompt_entry.text().to_string().replace('"', "");
            send(&format!("{{\"set\":{{\"prompt\":\"{text}\"}}}}"));
        }
    });
    paste.connect_state_set(|_, on| {
        send(&format!("{{\"set\":{{\"auto_paste\":{on}}}}}"));
        glib::Propagation::Proceed
    });

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 12);
    vbox.set_margin_top(14);
    vbox.set_margin_bottom(14);
    vbox.set_margin_start(14);
    vbox.set_margin_end(14);
    vbox.append(&card_status);
    vbox.append(&card_model);
    vbox.append(&card_lang);
    vbox.append(&card_input);

    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&vbox)
        .build();

    // Живой опрос 1 Гц (дешевле волны: текста мало).
    let shared = std::sync::Arc::new(std::sync::Mutex::new((State::default(), false)));
    let shared_w = shared.clone();
    std::thread::spawn(move || loop {
        match send("{\"status\":true}") {
            Some(st) => {
                if let Ok(mut g) = shared_w.lock() {
                    *g = (st, true);
                }
            }
            None => {
                if let Ok(mut g) = shared_w.lock() {
                    *g = (State::default(), false);
                }
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    });

    glib::timeout_add_local(Duration::from_millis(500), move || {
        let (st, up) = shared
            .lock()
            .map(|g| g.clone())
            .unwrap_or((State::default(), false));
        start_btn.set_visible(!up);
        if !up {
            daemon_state.set_label("Демон: остановлен");
            last_lbl.set_label("");
            dot.set_label("●");
            dot.add_css_class("dim-label");
            dot.remove_css_class("success");
        } else {
            daemon_state.set_label("Демон: активен");
            if st.recording {
                last_lbl.set_label("● идёт запись…");
            } else if st.last_text.is_empty() {
                last_lbl.set_label("Ожидание горячей клавиши (Pause/F12)");
            } else {
                last_lbl.set_label(&format!("\"{}\" ({} мс, {})", st.last_text, st.last_ms, st.loaded_model));
            }
            dot.set_label("●");
            dot.add_css_class("success");
            dot.remove_css_class("dim-label");
            model_note.set_label(&format!("Загружена: {}", st.loaded_model));
        }
        glib::ControlFlow::Continue
    });

    // Копирование текста по клику не нужно; окно компактное.
    let _ = gdk::Display::default();

    let win = adw::ApplicationWindow::builder()
        .application(app)
        .title("OpenDictate")
        .default_width(440)
        .default_height(560)
        .content(&scroll)
        .build();
    win.present();
}

fn main() {
    let guard: Rc<RefCell<DaemonGuard>> = Rc::new(RefCell::new(DaemonGuard { child: None }));
    let app = adw::Application::builder()
        .application_id("com.opendictate.Gui")
        .build();
    {
        let guard = guard.clone();
        app.connect_activate(move |app| build_ui(app, guard.clone()));
    }
    {
        let guard = guard.clone();
        app.connect_shutdown(move |_| guard.borrow_mut().stop());
    }
    app.run();
}
