//! SettingsStore: user preferences.

use gpui::{Context, EventEmitter};
use spark_model::Settings;

#[derive(Debug, Clone)]
pub enum SettingsEvent {
    Changed,
}

pub struct SettingsStore {
    pub settings: Settings,
}

impl EventEmitter<SettingsEvent> for SettingsStore {}

impl SettingsStore {
    pub fn new() -> Self {
        Self {
            settings: Settings::default(),
        }
    }

    pub fn update(&mut self, settings: Settings, cx: &mut Context<Self>) {
        self.settings = settings;
        cx.notify();
    }
}
