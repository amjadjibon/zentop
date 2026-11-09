//! One validated, persisted source of preferences.
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};

fn defaults() -> Value {
    serde_json::from_str(include_str!("default-config.json")).unwrap()
}
// Only recognized fields are accepted. Old installations may have partial
// configs, so merge valid leaves into current defaults rather than replacing groups.
fn merge(config: &mut Value, patch: &Value) {
    for group in ["appearance", "behavior"] {
        let Some(values) = patch[group].as_object() else {
            continue;
        };
        for (key, value) in values {
            if group == "appearance" && key == "columnVisibility" {
                if let Some(columns) = value.as_object() {
                    for (column, visible) in columns {
                        if config[group][key].get(column).is_some() && visible.is_boolean() {
                            config[group][key][column] = visible.clone();
                        }
                    }
                }
                continue;
            }
            if group == "appearance" && key == "theme" {
                if value.as_str().is_some_and(crate::ui::theme::exists) {
                    config[group][key] = value.clone();
                }
                continue;
            }
            let valid = match key.as_str() {
                "itemsPerPage" => value
                    .as_u64()
                    .is_some_and(|n| [15, 25, 50, 100, 250, 500].contains(&n)),
                "refreshRate" => value.as_u64().is_some_and(|n| (1000..=30000).contains(&n)),
                "menuBarEnabled" | "keepRunning" => value.is_boolean(),
                "processView" => matches!(value.as_str(), Some("flat" | "tree")),
                _ => false,
            };
            if group == "behavior" && valid {
                config[group][key] = value.clone();
            }
        }
    }
    config["appearance"]["columnVisibility"]["name"] = Value::Bool(true);
}
fn persist(path: &Path, config: &Value) -> Result<(), String> {
    let parent = path.parent().ok_or("Settings location unavailable")?;
    fs::create_dir_all(parent).map_err(|e| format!("Could not create settings folder: {e}"))?;
    let temporary = path.with_extension("json.tmp");
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(config).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("Could not save settings: {e}"))?;
    fs::rename(&temporary, path).map_err(|e| {
        let _ = fs::remove_file(&temporary);
        format!("Could not replace settings: {e}")
    })
}
/// Reads the settings file, filling gaps with defaults. On first launch the
/// file doesn't exist yet, so the defaults are saved to it.
fn load(path: &Path) -> Result<Value, String> {
    let existing = match fs::read(path) {
        Ok(bytes) => Some(
            serde_json::from_slice::<Value>(&bytes)
                .map_err(|e| format!("Could not read settings: {e}"))?,
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("Could not read settings: {e}")),
    };
    let mut config = defaults();
    match existing {
        Some(saved) => merge(&mut config, &saved),
        None => persist(path, &config)?,
    }
    Ok(config)
}
fn config_dir() -> Option<PathBuf> {
    zentop_core::monitoring_preferences::config_directory()
}

/// The persisted preferences file and its validated contents.
pub struct Preferences {
    path: Option<PathBuf>,
    config: Value,
}

impl Preferences {
    /// Loads saved settings, writing defaults on first launch. A missing config
    /// directory or unreadable file falls back to defaults and reports why.
    pub fn open() -> (Self, Option<String>) {
        let Some(path) = config_dir().map(|dir| dir.join("settings.json")) else {
            return (
                Self {
                    path: None,
                    config: defaults(),
                },
                Some("Settings location unavailable; changes will not be saved.".into()),
            );
        };
        match load(&path) {
            Ok(config) => (
                Self {
                    path: Some(path),
                    config,
                },
                None,
            ),
            Err(error) => (
                Self {
                    path: None,
                    config: defaults(),
                },
                Some(error),
            ),
        }
    }

    pub fn appearance(&self, key: &str) -> &Value {
        &self.config["appearance"][key]
    }

    pub fn behavior(&self, key: &str) -> &Value {
        &self.config["behavior"][key]
    }

    pub fn column_visible(&self, column: &str) -> Option<bool> {
        self.config["appearance"]["columnVisibility"][column].as_bool()
    }

    /// Validates and persists a partial update. On failure nothing changes.
    pub fn update(&mut self, patch: Value) -> Result<(), String> {
        let mut next = self.config.clone();
        merge(&mut next, &patch);
        if next == self.config {
            return Ok(());
        }
        let path = self
            .path
            .as_ref()
            .ok_or("Settings could not be loaded, so changes cannot be saved.")?;
        persist(path, &next)?;
        self.config = next;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn partial_configs_keep_defaults_and_reject_invalid_values() {
        let mut config = defaults();
        merge(
            &mut config,
            &json!({"behavior":{"refreshRate":5000,"keepRunning":true,"usageAnalytics":true,"itemsPerPage":0,"processView":"bad"},"appearance":{"columnVisibility":{"name":false,"pid":false,"unknown":true}}}),
        );
        assert_eq!(config["behavior"]["refreshRate"], 5000);
        assert!(config["behavior"].get("usageAnalytics").is_none());
        assert_eq!(config["behavior"]["itemsPerPage"], 15);
        assert_eq!(config["behavior"]["processView"], "flat");
        assert_eq!(config["appearance"]["columnVisibility"]["name"], true);
        assert_eq!(config["appearance"]["columnVisibility"]["pid"], false);
        assert!(config["appearance"]["columnVisibility"]
            .get("unknown")
            .is_none());
        merge(&mut config, &json!({"appearance":{"theme":"dracula"}}));
        assert_eq!(config["appearance"]["theme"], "dracula");
        merge(&mut config, &json!({"appearance":{"theme":"../../etc"}}));
        assert_eq!(config["appearance"]["theme"], "dracula");
    }
    #[test]
    fn independent_patches_do_not_overwrite_other_windows() {
        let mut config = defaults();
        merge(&mut config, &json!({"behavior":{"refreshRate":5000}}));
        merge(&mut config, &json!({"behavior":{"processView":"tree"}}));
        merge(
            &mut config,
            &json!({"appearance":{"columnVisibility":{"pid":false}}}),
        );
        assert_eq!(config["behavior"]["refreshRate"], 5000);
        assert_eq!(config["behavior"]["processView"], "tree");
    }
    #[test]
    fn first_launch_saves_defaults_and_failed_save_keeps_original() {
        let dir = std::env::temp_dir().join(format!("zentop-preferences-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        assert_eq!(load(&path).unwrap(), defaults());
        assert!(path.exists());
        let mut config = defaults();
        merge(&mut config, &json!({"behavior":{"refreshRate":5000}}));
        persist(&path, &config).unwrap();
        assert_eq!(load(&path).unwrap(), config);
        assert!(persist(&dir, &defaults()).is_err());
        assert_eq!(load(&path).unwrap(), config);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn retired_default_status_is_ignored() {
        // Retired preference: the status filter is per session.
        let mut config = defaults();
        merge(
            &mut config,
            &json!({"behavior": {"defaultStatusFilter": "sleeping"}}),
        );
        assert!(config["behavior"].get("defaultStatusFilter").is_none());
    }
}
