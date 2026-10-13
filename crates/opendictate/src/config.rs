//! Персистентный конфиг: `~/.config/opendictate/config.toml`.
//!
//! Приоритет значений: CLI-флаги > конфиг-файл > встроенные дефолты.
//! Демон пишет файл при смене настроек из трея/GUI (control-сокет),
//! поэтому выбор переживает рестарты.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum HotkeyMode {
    /// Удерживать клавишу, пока говоришь.
    #[default]
    Push,
    /// Нажал — пишет, нажал ещё раз — вставил.
    Toggle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Hotkey {
    #[default]
    Pause,
    F12,
    ScrollLock,
}

impl Hotkey {
    pub fn evdev_key(self) -> evdev::Key {
        match self {
            Hotkey::Pause => evdev::Key::KEY_PAUSE,
            Hotkey::F12 => evdev::Key::KEY_F12,
            Hotkey::ScrollLock => evdev::Key::KEY_SCROLLLOCK,
        }
    }

    pub fn all() -> &'static [(&'static str, Hotkey)] {
        &[
            ("pause", Hotkey::Pause),
            ("f12", Hotkey::F12),
            ("scrolllock", Hotkey::ScrollLock),
        ]
    }

    pub fn parse(s: &str) -> Option<Hotkey> {
        Self::all()
            .iter()
            .find(|(n, _)| *n == s.to_lowercase())
            .map(|(_, k)| *k)
    }

    pub fn name(self) -> &'static str {
        match self {
            Hotkey::Pause => "pause",
            Hotkey::F12 => "f12",
            Hotkey::ScrollLock => "scrolllock",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    pub model: Option<String>,
    pub language: Option<String>,
    pub auto_paste: Option<bool>,
    pub prompt: Option<String>,
    pub hotkey_mode: Option<HotkeyMode>,
    pub hotkey: Option<Hotkey>,
}

pub fn config_path() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(dir).join("opendictate/config.toml");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config/opendictate/config.toml")
}

pub fn load() -> Config {
    let path = config_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return Config::default(),
    };
    match toml::from_str(&text) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("[config] Не могу распарсить {}: {e:#}, использую дефолты", path.display());
            Config::default()
        }
    }
}

pub fn save(cfg: &Config) -> Result<()> {
    let path = config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = toml::to_string_pretty(cfg)?;
    std::fs::write(&path, text)?;
    Ok(())
}
