//! Sustained resource thresholds, bounded alert history and optional OS delivery.
use crate::sampling::Frame;
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Cpu,
    Memory,
    DiskFree,
    Temperature,
}
impl Kind {
    pub const ALL: [Self; 4] = [Self::Cpu, Self::Memory, Self::DiskFree, Self::Temperature];
    pub fn label(self) -> &'static str {
        match self {
            Self::Cpu => "CPU usage",
            Self::Memory => "Memory usage",
            Self::DiskFree => "Disk free space",
            Self::Temperature => "Temperature",
        }
    }
    fn index(self) -> usize {
        match self {
            Self::Cpu => 0,
            Self::Memory => 1,
            Self::DiskFree => 2,
            Self::Temperature => 3,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub enabled: bool,
    pub threshold: u8,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub cpu: Rule,
    pub memory: Rule,
    pub disk_free: Rule,
    pub temperature: Rule,
    pub sustain_secs: u64,
    pub cooldown_secs: u64,
    pub desktop_notifications: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            cpu: Rule {
                enabled: false,
                threshold: 90,
            },
            memory: Rule {
                enabled: false,
                threshold: 90,
            },
            disk_free: Rule {
                enabled: false,
                threshold: 10,
            },
            temperature: Rule {
                enabled: false,
                threshold: 90,
            },
            sustain_secs: 30,
            cooldown_secs: 300,
            desktop_notifications: false,
        }
    }
}
impl Settings {
    pub fn rule(&self, kind: Kind) -> &Rule {
        match kind {
            Kind::Cpu => &self.cpu,
            Kind::Memory => &self.memory,
            Kind::DiskFree => &self.disk_free,
            Kind::Temperature => &self.temperature,
        }
    }
    pub fn rule_mut(&mut self, kind: Kind) -> &mut Rule {
        match kind {
            Kind::Cpu => &mut self.cpu,
            Kind::Memory => &mut self.memory,
            Kind::DiskFree => &mut self.disk_free,
            Kind::Temperature => &mut self.temperature,
        }
    }
    pub fn enabled(&self) -> bool {
        Kind::ALL.iter().any(|k| self.rule(*k).enabled)
    }
    pub fn validate(&self) -> Result<(), String> {
        if Kind::ALL.iter().any(|k| {
            !(1..=if *k == Kind::Temperature { 150 } else { 99 }).contains(&self.rule(*k).threshold)
        }) {
            return Err("Alert thresholds must be 1–99% or 1–150°C for temperature.".into());
        }
        if !(1..=3600).contains(&self.sustain_secs) || !(1..=86400).contains(&self.cooldown_secs) {
            return Err(
                "Alert duration must be 1–3600 seconds; cooldown must be 1–86400 seconds.".into(),
            );
        }
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub struct Alert {
    pub kind: Kind,
    pub timestamp: u64,
    pub message: String,
}
#[derive(Default)]
struct State {
    since: Option<Instant>,
    last_fired: Option<Instant>,
    scope: String,
}
#[derive(Default)]
pub struct Center {
    settings: Settings,
    states: [State; 4],
    last_sequence: Option<u64>,
    last_sample: Option<Instant>,
    pub history: VecDeque<Alert>,
}
impl Center {
    /// Break pending durations on pauses/errors; retain history and cooldowns.
    pub fn suspend(&mut self) {
        for state in &mut self.states {
            state.since = None;
        }
        self.last_sample = None;
    }
    pub fn observe(
        &mut self,
        frame: &Frame,
        valid: bool,
        interval: Duration,
        settings: &Settings,
        now: Instant,
    ) -> Vec<Alert> {
        if self.settings != *settings {
            self.suspend();
            self.settings = settings.clone();
        }
        if !valid || settings.validate().is_err() {
            self.suspend();
            return vec![];
        }
        if self.last_sequence == Some(frame.sequence) {
            return vec![];
        }
        if self
            .last_sample
            .is_some_and(|last| now.saturating_duration_since(last) > interval.mul_f64(2.5))
        {
            self.suspend();
        }
        self.last_sample = Some(now);
        self.last_sequence = Some(frame.sequence);
        let stats = &frame.stats;
        let cpu = (frame.sequence > 1
            && !stats.cpu_usage.is_empty()
            && stats
                .cpu_usage
                .iter()
                .all(|v| v.is_finite() && (0.0..=100.0).contains(v)))
        .then(|| {
            stats.cpu_usage.iter().map(|v| *v as f64).sum::<f64>() / stats.cpu_usage.len() as f64
        });
        let memory = (stats.memory_total > 0 && stats.memory_used <= stats.memory_total)
            .then(|| stats.memory_used as f64 / stats.memory_total as f64 * 100.0);
        let volume = stats
            .details
            .volumes
            .iter()
            .filter(|v| !v.read_only && v.total > 0 && v.available <= v.total)
            .min_by(|a, b| {
                (a.available as f64 / a.total as f64)
                    .total_cmp(&(b.available as f64 / b.total as f64))
                    .then_with(|| a.mount.cmp(&b.mount))
            });
        let hot = crate::temperature::hottest(&stats.temperatures);
        let values = [
            (cpu, String::new()),
            (memory, String::new()),
            (
                volume.map(|v| v.available as f64 / v.total as f64 * 100.0),
                volume.map_or_else(String::new, |v| v.mount.clone()),
            ),
            (
                hot.and_then(|s| s.celsius),
                hot.map_or_else(String::new, |s| s.id.clone()),
            ),
        ];
        let mut fired = Vec::new();
        for (kind, (value, scope)) in Kind::ALL.into_iter().zip(values) {
            let rule = settings.rule(kind);
            let state = &mut self.states[kind.index()];
            if state.scope != scope {
                state.since = None;
                state.scope = scope;
            }
            let breached = value.is_some_and(|v| {
                if kind == Kind::DiskFree {
                    v <= rule.threshold as f64
                } else {
                    v >= rule.threshold as f64
                }
            });
            if !rule.enabled || !breached {
                state.since = None;
                continue;
            }
            let since = *state.since.get_or_insert(now);
            if now.saturating_duration_since(since) < Duration::from_secs(settings.sustain_secs)
                || state.last_fired.is_some_and(|last| {
                    now.saturating_duration_since(last)
                        < Duration::from_secs(settings.cooldown_secs)
                })
            {
                continue;
            }
            state.last_fired = Some(now);
            let value = value.unwrap_or_default();
            let message = if kind == Kind::DiskFree {
                format!(
                    "{}: {value:.1}% free (limit {}%) for {}s",
                    state.scope, rule.threshold, settings.sustain_secs
                )
            } else if kind == Kind::Temperature {
                format!(
                    "{}: {value:.1}°C (limit {}°C) for {}s",
                    hot.map_or("Temperature", |s| s.name.as_str()),
                    rule.threshold,
                    settings.sustain_secs
                )
            } else {
                format!(
                    "{}: {value:.1}% (limit {}%) for {}s",
                    kind.label(),
                    rule.threshold,
                    settings.sustain_secs
                )
            };
            let alert = Alert {
                kind,
                timestamp: frame.captured_at,
                message,
            };
            self.history.push_front(alert.clone());
            self.history.truncate(50);
            fired.push(alert);
        }
        fired
    }
}

#[cfg(all(feature = "notifications", target_os = "macos"))]
mod desktop_macos;

#[cfg(all(feature = "notifications", not(target_os = "macos")))]
fn send_desktop(message: &str) -> Result<(), String> {
    notify_rust::Notification::new()
        .appname("Zentop")
        .summary("Zentop resource alert")
        .body(message)
        .show()
        .map(|_| ())
        .map_err(|e| format!("Desktop notification failed: {e}"))
}

/// Bounded, lazy OS notification worker. Never blocks the sampling or UI thread.
#[cfg(feature = "notifications")]
#[derive(Default)]
pub struct DesktopNotifications {
    sender: Option<std::sync::mpsc::SyncSender<String>>,
    results: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
}
#[cfg(feature = "notifications")]
impl DesktopNotifications {
    pub fn send(&mut self, message: String) -> Result<(), String> {
        if self.sender.is_none() {
            let (tx, rx) = std::sync::mpsc::sync_channel::<String>(8);
            let (done, results) = std::sync::mpsc::sync_channel(8);
            std::thread::Builder::new()
                .name("zentop-alerts".into())
                .spawn(move || {
                    while let Ok(message) = rx.recv() {
                        #[cfg(target_os = "macos")]
                        let result = desktop_macos::send(&message);
                        #[cfg(not(target_os = "macos"))]
                        let result = send_desktop(&message);
                        let _ = done.try_send(result);
                    }
                })
                .map_err(|e| format!("Could not start desktop notifications: {e}"))?;
            self.sender = Some(tx);
            self.results = Some(results);
        }
        self.sender
            .as_ref()
            .ok_or("Desktop notifications unavailable")?
            .try_send(message)
            .map_err(|e| format!("Desktop notification not queued: {e}"))
    }
    /// Return a batch's delivery status. Keep any failure visible, and clear
    /// a previous error when a later batch succeeds.
    pub fn poll_result(&self) -> Option<Result<(), String>> {
        let mut status: Option<Result<(), String>> = None;
        if let Some(results) = &self.results {
            while let Ok(result) = results.try_recv() {
                if status.as_ref().is_none_or(Result::is_ok) {
                    status = Some(result);
                }
            }
        }
        status
    }
}

#[cfg(test)]
mod tests;
