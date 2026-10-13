//! Ввод напечатанного текста в активное окно.
//!
//! Использует два взаимодополняющих механизма:
//! 1. `wl-copy` (системный Wayland буфер обмена) — работает быстро, без root и udev-прав,
//!    поддерживает русский язык в любых кодировках и длинные абзацы.
//! 2. `/dev/uinput` — виртуальное устройство клавиатуры для прямой посимвольной печати.
//! 3. Автовставка Ctrl+V через uinput — layout-независимая вставка из буфера.

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

/// Автоматическая вставка из буфера обмена эмуляцией Ctrl+V.
///
/// Layout-независима (кириллица safe): печатает само приложение из буфера.
/// Требует доступа к /dev/uinput (udev-правило из assets/, иначе fallback на ручной Ctrl+V).
pub struct AutoPaster {
    dev: Option<evdev::uinput::VirtualDevice>,
    warned: bool,
}

impl AutoPaster {
    pub fn new() -> Self {
        match Self::open() {
            Ok(dev) => Self {
                dev: Some(dev),
                warned: false,
            },
            Err(e) => {
                eprintln!("[input] uinput недоступен ({e:#}); автовставка выключена, жмите Ctrl+V вручную");
                eprintln!("[input] hint: sudo install -m644 assets/90-opendictate.rules /etc/udev/rules.d/ && sudo udevadm trigger /dev/uinput");
                Self {
                    dev: None,
                    warned: true,
                }
            }
        }
    }

    fn open() -> Result<evdev::uinput::VirtualDevice> {
        use evdev::{AttributeSet, Key};
        let mut keys = AttributeSet::<Key>::new();
        keys.insert(Key::KEY_LEFTCTRL);
        keys.insert(Key::KEY_V);
        let dev = evdev::uinput::VirtualDeviceBuilder::new()?
            .name("opendictate-paste")
            .with_keys(&keys)?
            .build()
            .context("не могу открыть /dev/uinput")?;
        Ok(dev)
    }

    pub fn is_available(&self) -> bool {
        self.dev.is_some()
    }

    /// Нажать Ctrl+V с паузами, чтобы активное окно успело обработать.
    pub fn paste(&mut self) -> Result<()> {
        use evdev::{EventType, InputEvent, Key};
        let dev = self.dev.as_mut().context("uinput недоступен")?;
        let ev = |k: Key, v: i32| InputEvent::new(EventType::KEY, k.code(), v);
        dev.emit(&[ev(Key::KEY_LEFTCTRL, 1)])?;
        std::thread::sleep(std::time::Duration::from_millis(15));
        dev.emit(&[ev(Key::KEY_V, 1)])?;
        std::thread::sleep(std::time::Duration::from_millis(15));
        dev.emit(&[ev(Key::KEY_V, 0)])?;
        std::thread::sleep(std::time::Duration::from_millis(15));
        dev.emit(&[ev(Key::KEY_LEFTCTRL, 0)])?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn warned(&self) -> bool {
        self.warned
    }
}
