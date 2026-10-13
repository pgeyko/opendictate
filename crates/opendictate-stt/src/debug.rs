//! Модуль отладочного логирования для OpenDictate.
//!
//! При запуске с флагом `--debug` / `-d` все внутренние события выводятся
//! в stderr: подключение, запись, запуск whisper-cli, время работы, текст.
//! В обычном режиме лог тихий — только ключевые события.

use std::sync::atomic::{AtomicBool, Ordering};

static DEBUG_ENABLED: AtomicBool = AtomicBool::new(false);

/// Включить/выключить отладочный режим (из main при флаге `--debug`).
pub fn set_debug(enabled: bool) {
    DEBUG_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Включён ли отладочный режим.
pub fn is_debug() -> bool {
    DEBUG_ENABLED.load(Ordering::Relaxed)
}

/// Отладочное сообщение — печатается только при `--debug`.
pub fn debug_msg(msg: &str) {
    if is_debug() {
        eprintln!("[debug] {msg}");
    }
}

/// Информационное сообщение — печатается всегда (важные события).
pub fn info_msg(msg: &str) {
    eprintln!("[info] {msg}");
}
