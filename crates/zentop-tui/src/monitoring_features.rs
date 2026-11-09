//! Persistent views and resource-alert controls for the terminal frontend.
use crate::app::{App, Effect, Panel};
use ratatui::crossterm::event::KeyCode;
use zentop_core::{
    alerts::Kind,
    monitoring_preferences::{SavedView, Store},
};

impl App {
    pub fn load_features(&mut self) {
        let (features, error) = Store::open();
        self.features = features;
        self.feature_error = error;
    }
    pub fn capture_view(&self, name: String) -> SavedView {
        SavedView {
            name,
            search: self.search.clone(),
            filters: self.filters.clone(),
            sort: self.sort,
            columns: self.columns.clone(),
            tree: self.tree,
            pinned: self.pinned.iter().cloned().collect(),
        }
    }
    pub fn restore_view(&mut self, view: &SavedView) {
        self.search = view.search.clone();
        self.filters = view.filters.clone();
        self.sort = view.sort;
        self.columns = view.columns.clone();
        self.tree = view.tree;
        self.pinned = view.pinned.iter().cloned().collect();
        self.expanded.clear();
        self.searching = false;
        self.table.select(Some(0));
        self.rebuild(None);
    }
    pub fn name_key(&mut self, code: KeyCode) -> Effect {
        match code {
            KeyCode::Esc => self.view_name = None,
            KeyCode::Enter => {
                if let Some(name) = &self.view_name {
                    let view = self.capture_view(name.clone());
                    let saved_name = view.name.trim().to_lowercase();
                    self.feature_error = self.features.save_view(view).err();
                    if self.feature_error.is_none() {
                        self.view_name = None;
                        self.panel_index = self
                            .features
                            .config
                            .views
                            .iter()
                            .position(|v| v.name.to_lowercase() == saved_name)
                            .unwrap_or(0);
                    }
                }
            }
            KeyCode::Backspace => {
                if let Some(name) = &mut self.view_name {
                    name.pop();
                }
            }
            KeyCode::Char(c) if !c.is_control() => {
                if let Some(name) = &mut self.view_name {
                    if name.chars().count() < 64 {
                        name.push(c);
                    }
                }
            }
            _ => {}
        }
        Effect::None
    }
    pub fn features_key(&mut self, panel: Panel, code: KeyCode) -> Effect {
        let limit = if panel == Panel::Alerts {
            5
        } else {
            self.features.config.views.len().saturating_sub(1)
        };
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.panel_index = self.panel_index.saturating_sub(1);
                self.detail_scroll = 0;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.panel_index = (self.panel_index + 1).min(limit);
                self.detail_scroll = 0;
            }
            KeyCode::PageDown => self.detail_scroll = self.detail_scroll.saturating_add(8),
            KeyCode::PageUp => self.detail_scroll = self.detail_scroll.saturating_sub(8),
            _ => {}
        }
        if panel == Panel::Views {
            let selected = self.features.config.views.get(self.panel_index).cloned();
            match code {
                KeyCode::Char('n') => self.view_name = Some(String::new()),
                KeyCode::Char('u') => {
                    if let Some(view) = selected {
                        self.feature_error =
                            self.features.save_view(self.capture_view(view.name)).err();
                    }
                }
                KeyCode::Enter => {
                    if let Some(view) = selected {
                        self.restore_view(&view);
                        self.panel = None;
                    }
                }
                KeyCode::Delete | KeyCode::Char('d') => {
                    if let Some(view) = selected {
                        self.feature_error = self.features.delete_view(&view.name).err();
                        self.panel_index = self
                            .panel_index
                            .min(self.features.config.views.len().saturating_sub(1));
                    }
                }
                _ => {}
            }
        } else {
            let mut settings = self.features.config.alerts.clone();
            match code {
                KeyCode::Char('x') => self.alerts.history.clear(),
                KeyCode::Char(' ') | KeyCode::Enter => {
                    if self.panel_index < 3 {
                        let rule = settings.rule_mut(Kind::ALL[self.panel_index]);
                        rule.enabled = !rule.enabled;
                    } else if self.panel_index == 5 {
                        settings.desktop_notifications = !settings.desktop_notifications;
                    }
                }
                KeyCode::Left | KeyCode::Right => {
                    let add = code == KeyCode::Right;
                    if self.panel_index < 3 {
                        let rule = settings.rule_mut(Kind::ALL[self.panel_index]);
                        rule.threshold = if add {
                            rule.threshold.saturating_add(1)
                        } else {
                            rule.threshold.saturating_sub(1)
                        }
                        .clamp(1, 99);
                    } else if self.panel_index < 5 {
                        let (value, step, max) = if self.panel_index == 3 {
                            (&mut settings.sustain_secs, 5, 3600)
                        } else {
                            (&mut settings.cooldown_secs, 30, 86400)
                        };
                        *value = if add {
                            value.saturating_add(step)
                        } else {
                            value.saturating_sub(step)
                        }
                        .clamp(1, max);
                    }
                }
                _ => {}
            }
            if settings != self.features.config.alerts {
                self.feature_error = self.features.set_alerts(settings).err();
                self.alerts.suspend();
            }
        }
        Effect::None
    }
}
