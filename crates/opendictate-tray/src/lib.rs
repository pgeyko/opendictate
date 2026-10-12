//! Модуль системного трея (StatusNotifierItem) для Linux (Niri, GNOME, KDE, Waybar).

use ksni::blocking::TrayMethods;
use ksni::menu::{MenuItem, StandardItem};
use ksni::{Status, Tray};
use std::sync::{Arc, Mutex};

pub struct DictateTray {
    pub is_recording: Arc<Mutex<bool>>,
    pub language: Arc<Mutex<String>>,
}

impl Tray for DictateTray {
    fn id(&self) -> String {
        "opendictate".into()
    }

    fn title(&self) -> String {
        "OpenDictate".into()
    }

    fn status(&self) -> Status {
        Status::Active
    }

    fn icon_name(&self) -> String {
        let rec = self.is_recording.lock().map(|r| *r).unwrap_or(false);
        if rec {
            "media-record".into()
        } else {
            "audio-input-microphone".into()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let lang = self.language.lock().map(|l| l.clone()).unwrap_or_else(|_| "ru".into());
        vec![
            StandardItem {
                label: format!("OpenDictate (Язык: {})", lang),
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Русский (ru)".into(),
                activate: Box::new(|this: &mut Self| {
                    if let Ok(mut l) = this.language.lock() {
                        *l = "ru".into();
                    }
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "English (en)".into(),
                activate: Box::new(|this: &mut Self| {
                    if let Ok(mut l) = this.language.lock() {
                        *l = "en".into();
                    }
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Выход".into(),
                activate: Box::new(|_| {
                    std::process::exit(0);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

pub fn spawn_tray(
    is_recording: Arc<Mutex<bool>>,
    language: Arc<Mutex<String>>,
) -> anyhow::Result<ksni::blocking::Handle<DictateTray>> {
    let tray = DictateTray {
        is_recording,
        language,
    };
    let handle = tray.spawn()?;
    Ok(handle)
}
