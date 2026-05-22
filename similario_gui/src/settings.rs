use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GuiSettings {
    // Compare config
    pub tolerance: f32,
    pub duration_tolerance_pct: f32,
    pub min_matching_windows: f32,
    pub subclip_min_match: f32,
    // Signature config
    pub window_count: i32,
    pub skip_secs: i32,
    pub cropdetect: bool,
    pub audio_fingerprint: bool,
    pub audio_max_difference: f32,
    pub audio_min_segment_duration: f32,
    // UI state
    pub last_directory: String,
    pub dark_theme: bool,
}

impl Default for GuiSettings {
    fn default() -> Self {
        Self {
            tolerance: 0.15,
            duration_tolerance_pct: 20.0,
            min_matching_windows: 0.6,
            subclip_min_match: 0.5,
            window_count: 5,
            skip_secs: 15,
            cropdetect: true,
            audio_fingerprint: false,
            audio_max_difference: 3.0,
            audio_min_segment_duration: 5.0,
            last_directory: String::new(),
            dark_theme: true,
        }
    }
}

fn settings_path() -> PathBuf {
    let config_dir = std::env::var("XDG_CONFIG_HOME").map_or_else(
        |_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".config")
        },
        PathBuf::from,
    );
    config_dir.join("similario").join("settings.json")
}

impl GuiSettings {
    pub fn load() -> Self {
        let path = settings_path();
        match std::fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let path = settings_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    log::warn!("Failed to save settings to {}: {e}", path.display());
                }
            }
            Err(e) => log::warn!("Failed to serialize settings: {e}"),
        }
    }
}
