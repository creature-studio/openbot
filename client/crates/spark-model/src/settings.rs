use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub server_url: String,
    pub theme: Theme,
    pub font_size: f32,
    pub font_family: String,
    pub show_browser_panel: bool,
    pub show_terminal_panel: bool,
    pub show_files_panel: bool,
    pub auto_expand_running_tools: bool,
    pub timeline_virtualize: bool,
    /// Saved machines (local + SSH). SSH credentials are NOT stored here —
    /// this only stores display info. Real auth uses ~/.ssh/config + ssh-agent.
    pub machines: Vec<Machine>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Theme {
    Dark,
    Light,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            server_url: "http://localhost:3456".to_string(),
            theme: Theme::Dark,
            font_size: 14.0,
            font_family: "Zed Mono".to_string(),
            show_browser_panel: true,
            show_terminal_panel: true,
            show_files_panel: true,
            auto_expand_running_tools: true,
            timeline_virtualize: true,
            machines: vec![],
        }
    }
}
