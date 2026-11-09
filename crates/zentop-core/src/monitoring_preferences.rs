//! Shared, validated alerts and named process views. Never stores collected readings.
use crate::{
    alerts,
    process_view::{Column, Filters, RuleKind, Sort, STATUSES},
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedView {
    pub name: String,
    pub search: String,
    pub filters: Filters,
    pub sort: Sort,
    pub columns: Vec<Column>,
    pub tree: bool,
    pub pinned: Vec<String>,
}
impl SavedView {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty()
            || self.name.chars().count() > 64
            || self.name.chars().any(char::is_control)
        {
            return Err(
                "View name must contain 1–64 characters without control characters.".into(),
            );
        }
        if self.search.len() > 4096 || self.search.chars().any(char::is_control) {
            return Err("View search is too long or contains control characters.".into());
        }
        if self.columns.is_empty()
            || self.columns.len() > Column::ALL.len()
            || !self.columns.contains(&Column::Name)
            || self
                .columns
                .iter()
                .enumerate()
                .any(|(i, c)| self.columns[..i].contains(c))
        {
            return Err("Views require the Name column and unique columns.".into());
        }
        if RuleKind::ALL.iter().any(|k| {
            let v = self.filters.rule(*k).value;
            !v.is_finite() || !(0.0..=1e12).contains(&v)
        }) || self
            .filters
            .statuses
            .iter()
            .any(|s| !STATUSES.contains(&s.as_str()))
            || self.filters.statuses.len() > STATUSES.len()
        {
            return Err("View filters contain invalid values.".into());
        }
        if self.pinned.len() > 256 || self.pinned.iter().any(|s| s.len() > 16384) {
            return Err("A view can contain at most 256 pinned commands of 16 KiB each.".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Configuration {
    pub version: u32,
    pub alerts: alerts::Settings,
    pub views: Vec<SavedView>,
}
impl Default for Configuration {
    fn default() -> Self {
        Self {
            version: 1,
            alerts: alerts::Settings::default(),
            views: vec![],
        }
    }
}
impl Configuration {
    fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err("Unsupported monitoring settings version.".into());
        }
        self.alerts.validate()?;
        if self.views.len() > 32 {
            return Err("At most 32 monitoring views can be saved.".into());
        }
        let mut names = HashSet::new();
        for view in &self.views {
            view.validate()?;
            if !names.insert(view.name.trim().to_lowercase()) {
                return Err("Saved view names must be unique.".into());
            }
        }
        Ok(())
    }
}
fn load(path: &Path) -> Result<Configuration, String> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Configuration::default()),
        Err(e) => return Err(format!("Could not read monitoring settings: {e}")),
    };
    let mut data = Vec::new();
    file.take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    if data.len() > 2 * 1024 * 1024 {
        return Err("Monitoring settings exceed 2 MiB.".into());
    }
    let config: Configuration =
        serde_json::from_slice(&data).map_err(|e| format!("Invalid monitoring settings: {e}"))?;
    config.validate()?;
    Ok(config)
}
#[derive(Default)]
pub struct Store {
    path: Option<PathBuf>,
    pub config: Configuration,
}
/// Shared settings directory, with an optional isolated/portable profile.
pub fn config_directory() -> Option<PathBuf> {
    std::env::var_os("ZENTOP_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::config_dir().map(|p| p.join("com.zentop.app")))
}
impl Store {
    pub fn open() -> (Self, Option<String>) {
        match config_directory() {
            Some(path) => Self::open_at(path.join("monitoring.json")),
            None => (
                Self::default(),
                Some("Monitoring settings location unavailable.".into()),
            ),
        }
    }
    pub fn open_at(path: PathBuf) -> (Self, Option<String>) {
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
                    path: Some(path),
                    config: Configuration::default(),
                },
                Some(error),
            ),
        }
    }
    pub fn reload(&mut self) -> Result<(), String> {
        if let Some(path) = &self.path {
            self.config = load(path)?;
        }
        Ok(())
    }
    pub fn set_alerts(&mut self, settings: alerts::Settings) -> Result<(), String> {
        settings.validate()?;
        self.change(move |config| {
            config.alerts = settings;
            Ok(())
        })
    }
    pub fn save_view(&mut self, mut view: SavedView) -> Result<(), String> {
        view.name = view.name.trim().into();
        view.pinned.sort();
        view.pinned.dedup();
        view.validate()?;
        self.change(move |config| {
            if let Some(old) = config
                .views
                .iter_mut()
                .find(|v| v.name.to_lowercase() == view.name.to_lowercase())
            {
                *old = view;
            } else {
                config.views.push(view);
            }
            config.validate()
        })
    }
    pub fn delete_view(&mut self, name: &str) -> Result<(), String> {
        self.change(|config| {
            config
                .views
                .retain(|v| v.name.to_lowercase() != name.trim().to_lowercase());
            Ok(())
        })
    }
    fn change(
        &mut self,
        edit: impl FnOnce(&mut Configuration) -> Result<(), String>,
    ) -> Result<(), String> {
        let path = self
            .path
            .as_ref()
            .ok_or("Monitoring settings cannot be saved in this session.")?;
        let parent = path
            .parent()
            .ok_or("Monitoring settings location unavailable")?;
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let mut lock_options = OpenOptions::new();
        lock_options
            .create(true)
            .truncate(false)
            .read(true)
            .write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            lock_options.mode(0o600);
        }
        let lock = lock_options
            .open(path.with_extension("lock"))
            .map_err(|e| e.to_string())?;
        FileExt::lock_exclusive(&lock).map_err(|e| e.to_string())?;
        let mut next = load(path)?;
        edit(&mut next)?;
        next.validate()?;
        let data = serde_json::to_vec_pretty(&next).map_err(|e| e.to_string())?;
        if data.len() > 2 * 1024 * 1024 {
            return Err("Monitoring settings exceed 2 MiB.".into());
        }
        let temporary = path.with_extension("json.tmp");
        let result = (|| -> Result<(), String> {
            let mut options = OpenOptions::new();
            options.create(true).truncate(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary).map_err(|e| e.to_string())?;
            file.write_all(&data)
                .and_then(|_| file.sync_all())
                .map_err(|e| e.to_string())?;
            fs::rename(&temporary, path).map_err(|e| e.to_string())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result.map_err(|e| format!("Could not save monitoring settings: {e}"))?;
        self.config = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
