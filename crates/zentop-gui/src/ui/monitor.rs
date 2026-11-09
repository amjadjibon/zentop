//! The shared model: one sampling loop and one preferences file for every
//! window. Views observe this entity and re-render when it notifies.
use super::theme;
use crate::preferences::Preferences;
use crate::sampling::{self, Frame, Sample, Snapshot};
use crate::state::AppState;
use gpui_kit::{App, AppContext, Context, Entity, Global, Task};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};
use zentop_core::{
    alerts::{Center, DesktopNotifications, Settings as AlertSettings},
    monitoring_preferences::{SavedView, Store},
};

/// How often the loop checks whether a capture is due; captures stay interval-gated.
const TICK: Duration = Duration::from_millis(250);

pub const REFRESH_RATES: [u64; 5] = [2000, 3000, 5000, 10000, 30000];

pub struct Monitor {
    pub features: Store,
    pub feature_error: Option<String>,
    pub alerts: Center,
    pub notification_error: Option<String>,
    notifications: DesktopNotifications,
    state: Arc<AppState>,
    prefs: Preferences,
    pub prefs_error: Option<String>,
    pub frame: Option<Arc<Frame>>,
    pub history: Vec<Sample>,
    pub paused: bool,
    pub error: Option<String>,
    pub port_error: Option<String>,
    next_poll: Option<Instant>,
    sampling: bool,
    /// Per-process GPU usage and socket ports are only collected on demand.
    want_gpu: bool,
    want_ports: bool,
    _sampler: Task<()>,
}

struct GlobalMonitor(Entity<Monitor>);
impl Global for GlobalMonitor {}

pub fn init(cx: &mut App) -> Entity<Monitor> {
    let (prefs, prefs_error) = Preferences::open();
    theme::apply(
        prefs
            .appearance("theme")
            .as_str()
            .unwrap_or(theme::DEFAULT_THEME),
        cx,
    );
    let monitor = cx.new(|cx| Monitor::new(prefs, prefs_error, cx));
    cx.set_global(GlobalMonitor(monitor.clone()));
    monitor
}

pub fn global(cx: &App) -> Entity<Monitor> {
    cx.global::<GlobalMonitor>().0.clone()
}

impl Monitor {
    fn new(prefs: Preferences, prefs_error: Option<String>, cx: &mut Context<Self>) -> Self {
        let state = Arc::new(AppState::new());
        if let Ok(mut hub) = state.hub.lock() {
            hub.interval =
                sampling::interval(prefs.behavior("refreshRate").as_u64().unwrap_or(3000));
        }
        let sampler = cx.spawn(async move |this, cx| loop {
            // A hidden, minimized or fully occluded window still exists in
            // cx.windows(). Capture only while a window can show the result.
            let Ok(job) = this.update(cx, |this, cx| {
                let visible = cx.windows().into_iter().any(|handle| {
                    handle
                        .update(cx, |_, window, _| window.is_visible())
                        .unwrap_or(false)
                });
                if visible || this.features.config.alerts.enabled() {
                    this.poll_job()
                } else {
                    None
                }
            }) else {
                break;
            };
            if let Some((state, gpu, ports)) = job {
                let result = cx
                    .background_executor()
                    .spawn(async move { sample(&state, gpu, ports) })
                    .await;
                if this.update(cx, |this, cx| this.apply(result, cx)).is_err() {
                    break;
                }
            }
            cx.background_executor().timer(TICK).await;
        });
        let (features, feature_error) = Store::open();
        Self {
            features,
            feature_error,
            alerts: Center::default(),
            notification_error: None,
            notifications: DesktopNotifications::default(),
            state,
            prefs,
            prefs_error,
            frame: None,
            history: Vec::new(),
            paused: false,
            error: None,
            port_error: None,
            next_poll: None,
            sampling: false,
            want_gpu: false,
            want_ports: false,
            _sampler: sampler,
        }
    }

    pub fn reload_features(&mut self, cx: &mut Context<Self>) {
        self.feature_error = self.features.reload().err();
        cx.notify();
    }
    pub fn set_alerts(&mut self, settings: AlertSettings, cx: &mut Context<Self>) {
        self.feature_error = self.features.set_alerts(settings).err();
        self.alerts.suspend();
        cx.notify();
    }
    pub fn save_view(&mut self, view: SavedView, cx: &mut Context<Self>) {
        self.feature_error = self.features.save_view(view).err();
        cx.notify();
    }
    pub fn delete_view(&mut self, name: &str, cx: &mut Context<Self>) {
        self.feature_error = self.features.delete_view(name).err();
        cx.notify();
    }
    pub fn state(&self) -> Arc<AppState> {
        self.state.clone()
    }

    pub fn prefs(&self) -> &Preferences {
        &self.prefs
    }

    pub fn behavior_bool(&self, key: &str) -> bool {
        self.prefs.behavior(key).as_bool().unwrap_or(false)
    }

    pub fn refresh_rate(&self) -> u64 {
        self.prefs.behavior("refreshRate").as_u64().unwrap_or(3000)
    }

    pub fn theme_name(&self) -> &str {
        self.prefs
            .appearance("theme")
            .as_str()
            .unwrap_or(theme::DEFAULT_THEME)
    }

    /// Requests a capture on the next tick instead of waiting for the interval.
    pub fn refresh_now(&mut self) {
        self.next_poll = None;
    }

    /// What the main window currently needs beyond the base capture.
    pub fn set_demand(&mut self, gpu: bool, ports: bool) {
        let changed = (gpu && !self.want_gpu) || (ports && !self.want_ports);
        self.want_gpu = gpu;
        self.want_ports = ports;
        if changed {
            self.refresh_now();
        }
    }

    pub fn set_paused(&mut self, paused: bool, cx: &mut Context<Self>) {
        if let Ok(mut hub) = self.state.hub.lock() {
            hub.pause(paused);
        }
        self.alerts.suspend();
        self.paused = paused;
        self.refresh_now();
        cx.notify();
    }

    pub fn set_refresh_rate(&mut self, ms: u64, cx: &mut Context<Self>) {
        let interval = sampling::interval(ms);
        if let Ok(mut hub) = self.state.hub.lock() {
            hub.interval = interval;
        }
        self.next_poll = Some(Instant::now() + Duration::from_millis(interval));
        self.save(json!({"behavior": {"refreshRate": ms}}), cx);
    }

    pub fn set_theme(&mut self, name: &str, cx: &mut Context<Self>) {
        theme::apply(name, cx);
        self.save(json!({"appearance": {"theme": name}}), cx);
    }

    /// Validates and persists a partial preferences update, then notifies views.
    pub fn save(&mut self, patch: Value, cx: &mut Context<Self>) {
        self.prefs_error = self.prefs.update(patch).err();
        cx.notify();
    }

    fn poll_job(&mut self) -> Option<(Arc<AppState>, bool, bool)> {
        if self.sampling || self.next_poll.is_some_and(|due| Instant::now() < due) {
            return None;
        }
        let interval = self.state.hub.lock().map(|h| h.interval).unwrap_or(3000);
        self.next_poll = Some(Instant::now() + Duration::from_millis(interval));
        self.sampling = true;
        Some((self.state.clone(), self.want_gpu, self.want_ports))
    }

    fn apply(&mut self, result: Result<Snapshot, String>, cx: &mut Context<Self>) {
        self.sampling = false;
        if let Some(result) = self.notifications.poll_result() {
            self.notification_error = result.err();
        }
        match result {
            Ok(snapshot) if snapshot.paused == self.paused => {
                let settings = &self.features.config.alerts;
                let events = self.alerts.observe(
                    &snapshot.frame,
                    !snapshot.paused && snapshot.error.is_none(),
                    Duration::from_millis(self.refresh_rate()),
                    settings,
                    Instant::now(),
                );
                if settings.desktop_notifications {
                    for event in events {
                        if let Err(error) = self.notifications.send(event.message) {
                            self.notification_error = Some(error);
                        }
                    }
                }
                self.paused = snapshot.paused;
                self.error = snapshot.error;
                self.port_error = snapshot.port_error;
                self.history = snapshot.history;
                self.frame = Some(snapshot.frame);
            }
            Ok(_) => {
                // A capture queued before pause/resume must not advance alerts
                // or restore the previous pause state.
                self.alerts.suspend();
                self.refresh_now();
            }
            Err(error) => {
                self.alerts.suspend();
                self.error = Some(error);
            }
        }
        cx.notify();
    }
}

/// One capture through the shared, interval-gated hub.
fn sample(state: &AppState, gpu: bool, ports: bool) -> Result<Snapshot, String> {
    let mut hub = state.hub.lock().map_err(|e| e.to_string())?;
    hub.process_gpu = gpu;
    let mut snapshot = hub.read(state)?;
    drop(hub);
    if ports {
        snapshot.attach_ports();
    }
    Ok(snapshot)
}
