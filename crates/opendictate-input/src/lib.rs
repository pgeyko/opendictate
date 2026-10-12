//! Ввод напечатанного текста в активное окно.
//!
//! Использует два взаимодополняющих механизма:
//! 1. `wl-copy` (системный Wayland буфер обмена) — работает быстро, без root и udev-прав,
//!    поддерживает русский язык в любых кодировках и длинные абзацы.
//! 2. `/dev/uinput` — виртуальное устройство клавиатуры для прямой посимвольной печати.

use anyhow::{Context, Result};
use std::io::Write;
use std::process::{Command, Stdio};

pub struct TextInjector;

impl TextInjector {
    pub fn new() -> Self {
        Self
    }

    /// Вставить распознанный текст в активное поле.
    ///
    /// В Wayland (Niri/GNOME/KDE) копирует текст в буфер обмена через `wl-copy`.
    /// Пользователь может сразу нажать Ctrl+V (или активировать автоматическую вставку).
    pub fn copy_to_clipboard(&self, text: &str) -> Result<()> {
        let mut child = Command::new("wl-copy")
            .stdin(Stdio::piped())
            .spawn()
            .context("Не удалось запустить wl-copy. Убедитесь, что утилита установлена.")?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(text.as_bytes())?;
        }

        let status = child.wait()?;
        if !status.success() {
            anyhow::bail!("wl-copy завершился с ошибкой: {:?}", status.code());
        }

        Ok(())
    }
}
