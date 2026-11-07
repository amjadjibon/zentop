//! The menu-bar overview: essential readings and the busiest processes in a
//! small popup anchored to the tray icon. It closes when it loses focus.
use super::monitor::{self, Monitor};
use super::process_view::{format_bytes, format_mb, identity, Identity};
use super::{stats, theme, Windows, APP_ICON};
use crate::native_icons::{Icon, IconRequest};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    div, img, point, px, size, App, AppContext, Bounds, Context, Entity, FontWeight,
    InteractiveElement, IntoElement, ParentElement, Pixels, Render, StatefulInteractiveElement,
    Styled, Subscription, Window, WindowBackgroundAppearance, WindowBounds, WindowKind,
    WindowOptions,
};
use std::collections::HashMap;
use std::time::Instant;

const WIDTH: f32 = 340.;
const HEIGHT: f32 = 540.;
const BUSIEST: usize = 5;

/// The tray icon's screen rectangle, in physical pixels.
#[derive(Clone, Copy, Debug)]
pub struct Anchor {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

pub fn close(cx: &mut App) {
    if let Some(handle) = cx.global_mut::<Windows>().overview.take() {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
        cx.global_mut::<Windows>().overview_closed_at = Some(Instant::now());
    }
}

pub fn toggle(anchor: Option<Anchor>, cx: &mut App) {
    if cx.global::<Windows>().overview.is_some() {
        close(cx);
    } else if !super::recently_closed_overview(cx) {
        show(anchor, cx);
    }
}

/// Converts the tray rectangle to logical points and keeps the panel on screen.
fn placement(anchor: Option<Anchor>, cx: &mut App) -> Bounds<Pixels> {
    // Tray rectangles are physical; use the main window's scale when it is open.
    let main = cx
        .global::<Windows>()
        .main
        .as_ref()
        .map(|(handle, _)| *handle);
    let scale = main
        .and_then(|handle| handle.update(cx, |_, w, _| w.scale_factor()).ok())
        .unwrap_or(if cfg!(target_os = "macos") { 2.0 } else { 1.0 }) as f64;
    let panel = size(px(WIDTH), px(HEIGHT));
    let displays = cx.displays();
    let screen_of = |x: f32, y: f32| {
        displays
            .iter()
            .map(|d| d.bounds())
            .find(|b| b.contains(&point(px(x), px(y))))
            .or_else(|| cx.primary_display().map(|d| d.bounds()))
    };
    let (origin, screen) = match anchor {
        Some(a) => {
            let (x, y, w, h) = (
                (a.x / scale) as f32,
                (a.y / scale) as f32,
                (a.width / scale) as f32,
                (a.height / scale) as f32,
            );
            let screen = screen_of(x, y);
            let below = screen.is_none_or(|s| y + h + HEIGHT <= f32::from(s.bottom()));
            let top = if below { y + h + 4. } else { y - HEIGHT - 4. };
            (point(px(x + w / 2. - WIDTH / 2.), px(top)), screen)
        }
        None => {
            let screen = cx.primary_display().map(|d| d.bounds());
            let origin = screen.map_or(point(px(0.), px(0.)), |s| {
                point(s.right() - px(WIDTH + 8.), s.top() + px(32.))
            });
            (origin, screen)
        }
    };
    let origin = match screen {
        Some(s) => point(
            origin
                .x
                .clamp(s.left(), (s.right() - panel.width).max(s.left())),
            origin
                .y
                .clamp(s.top(), (s.bottom() - panel.height).max(s.top())),
        ),
        None => origin,
    };
    Bounds {
        origin,
        size: panel,
    }
}

pub fn show(anchor: Option<Anchor>, cx: &mut App) {
    if cx.global::<Windows>().overview.is_some() {
        return;
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(placement(anchor, cx))),
        titlebar: None,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        focus: true,
        show: true,
        window_background: WindowBackgroundAppearance::Transparent,
        app_id: Some("com.neohtop.dev".into()),
        ..Default::default()
    };
    if let Ok((handle, _)) = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| Overview::new(window, cx))
    }) {
        cx.global_mut::<Windows>().overview = Some(handle);
        let _ = handle.update(cx, |_, window, _| window.activate_window());
    }
    cx.activate(true);
}

pub struct Overview {
    monitor: Entity<Monitor>,
    icons: HashMap<Identity, Option<Icon>>,
    _subscriptions: Vec<Subscription>,
}

impl Overview {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let monitor = monitor::global(cx);
        let subscriptions = vec![
            cx.observe(&monitor, |this, _, cx| {
                this.load_icons(cx);
                cx.notify();
            }),
            cx.observe_window_activation(window, |_, window, cx| {
                if !window.is_window_active() {
                    cx.defer(close);
                }
            }),
        ];
        let mut this = Self {
            monitor,
            icons: HashMap::new(),
            _subscriptions: subscriptions,
        };
        this.load_icons(cx);
        this
    }

    fn busiest(&self, cx: &App) -> Vec<crate::monitoring::ProcessInfo> {
        let Some(frame) = self.monitor.read(cx).frame.clone() else {
            return Vec::new();
        };
        let mut processes: Vec<_> = frame.processes.iter().collect();
        processes.sort_by(|a, b| {
            b.cpu_usage
                .total_cmp(&a.cpu_usage)
                .then_with(|| b.memory_usage.cmp(&a.memory_usage))
                .then_with(|| a.pid.cmp(&b.pid))
        });
        processes.into_iter().take(BUSIEST).cloned().collect()
    }

    fn load_icons(&mut self, cx: &mut Context<Self>) {
        let requests: Vec<IconRequest> = self
            .busiest(cx)
            .iter()
            .map(identity)
            .filter(|id| !self.icons.contains_key(id))
            .map(|(pid, start_time)| IconRequest { pid, start_time })
            .collect();
        if requests.is_empty() {
            return;
        }
        for r in &requests {
            self.icons.insert((r.pid, r.start_time), None);
        }
        let state = self.monitor.read(cx).state();
        cx.spawn(async move |this, cx| {
            let resolved = cx
                .background_executor()
                .spawn(async move { crate::native_icons::resolve_batch(&state, requests) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.icons.extend(resolved);
                cx.notify();
            });
        })
        .detach();
    }
}

fn metric(label: &'static str, value: String, detail: String, cx: &App) -> impl IntoElement {
    v_flex()
        .flex_1()
        .min_w_0()
        .p_2()
        .rounded(cx.theme().radius)
        .bg(cx.theme().secondary)
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .child(value),
        )
        .child(
            div()
                .text_xs()
                .truncate()
                .text_color(cx.theme().muted_foreground)
                .child(detail),
        )
}

impl Render for Overview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let monitor = self.monitor.read(cx);
        let paused = monitor.paused;
        let frame = monitor.frame.clone();
        let history = monitor.history.clone();
        let unavailable = monitor.error.is_some();
        let busiest = self.busiest(cx);
        let palette = theme::palette(cx);
        let pause_monitor = self.monitor.clone();

        let readings = frame.as_ref().map(|frame| {
            let s = &frame.stats;
            let cpu = (!s.cpu_usage.is_empty()).then(|| {
                s.cpu_usage.iter().map(|v| *v as f64).sum::<f64>() / s.cpu_usage.len() as f64
            });
            let memory =
                (s.memory_total > 0).then(|| s.memory_used as f64 / s.memory_total as f64 * 100.0);
            let gpu = frame.gpus.first();
            let rate =
                |v: Option<u64>| v.map_or_else(|| "—".into(), |b| format!("{}/s", format_bytes(b)));
            v_flex()
                .gap_2()
                .child(
                    h_flex()
                        .gap_2()
                        .child(metric(
                            "CPU",
                            cpu.map_or_else(|| "—".into(), |v| format!("{v:.1}%")),
                            format!("{} cores", s.cpu_usage.len()),
                            cx,
                        ))
                        .child(metric(
                            "Memory",
                            memory.map_or_else(|| "—".into(), |v| format!("{v:.1}%")),
                            format!("{} used", format_bytes(s.memory_used)),
                            cx,
                        )),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(metric(
                            "GPU",
                            gpu.and_then(|g| g.utilization)
                                .map_or_else(|| "—".into(), |v| format!("{v:.1}%")),
                            gpu.map_or_else(|| "No readings".into(), |g| g.name.clone()),
                            cx,
                        ))
                        .child(metric(
                            "Network",
                            rate(s.network_rx_bytes),
                            format!("↑ {}", rate(s.network_tx_bytes)),
                            cx,
                        )),
                )
                .child(
                    h_flex()
                        .justify_between()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Free storage")
                        .child(format_bytes(s.disk_free_bytes)),
                )
                .child(div().h(px(36.)).flex().child(stats::trend(
                    &history,
                    |s| s.cpu,
                    palette.charts[0],
                    unavailable,
                )))
        });

        div().size_full().p_1().child(
            v_flex()
                .size_full()
                .gap_2()
                .p_3()
                .rounded(cx.theme().radius_lg)
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().background)
                .text_color(cx.theme().foreground)
                .shadow_lg()
                .child(
                    h_flex()
                        .justify_between()
                        .child(
                            h_flex()
                                .gap_2()
                                .child(img(APP_ICON.clone()).size_5())
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child("NeoHtop"),
                                )
                                .child(
                                    h_flex()
                                        .gap_1()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(div().size_1p5().rounded_full().bg(if paused {
                                            cx.theme().muted_foreground
                                        } else {
                                            palette.running
                                        }))
                                        .child(if paused { "Paused" } else { "Live" }),
                                ),
                        )
                        .child(
                            Button::new("overview-pause")
                                .ghost()
                                .xsmall()
                                .icon(if paused {
                                    IconName::Play
                                } else {
                                    IconName::Pause
                                })
                                .tooltip(if paused {
                                    "Resume updates"
                                } else {
                                    "Pause updates"
                                })
                                .on_click(move |_, _, cx| {
                                    pause_monitor.update(cx, |m, cx| m.set_paused(!paused, cx))
                                }),
                        ),
                )
                .children(readings)
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(cx.theme().muted_foreground)
                        .child("Busiest processes"),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_h_0()
                        .overflow_hidden()
                        .gap_0p5()
                        .children(busiest.into_iter().enumerate().map(|(i, p)| {
                            let id = identity(&p);
                            let icon = self.icons.get(&id).cloned().flatten();
                            h_flex()
                                .id(("busy", i))
                                .gap_2()
                                .px_2()
                                .py_1()
                                .rounded(cx.theme().radius)
                                .cursor_pointer()
                                .hover(|this| this.bg(cx.theme().accent))
                                .on_click(move |_, _, cx| {
                                    let id = id;
                                    cx.defer(move |cx| super::show_process(id, cx));
                                })
                                .child(match icon {
                                    Some(icon) => {
                                        img(icon).size_4().flex_shrink_0().into_any_element()
                                    }
                                    None => div()
                                        .size_4()
                                        .flex_shrink_0()
                                        .rounded_sm()
                                        .bg(cx.theme().muted)
                                        .into_any_element(),
                                })
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_sm()
                                        .child(p.name.clone()),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .when(p.cpu_usage > 50.0, |d| d.text_color(palette.danger))
                                        .child(format!("{:.1}%", p.cpu_usage)),
                                )
                                .child(
                                    div()
                                        .w(px(72.))
                                        .text_right()
                                        .text_xs()
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format_mb(p.memory_usage)),
                                )
                        })),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("open-main")
                                .primary()
                                .small()
                                .flex_1()
                                .label("Open NeoHtop")
                                .on_click(|_, _, cx| cx.defer(super::open_main)),
                        )
                        .child(
                            Button::new("overview-settings")
                                .ghost()
                                .small()
                                .icon(IconName::Settings)
                                .tooltip("Settings")
                                .on_click(|_, _, cx| cx.defer(super::open_settings)),
                        )
                        .child(
                            Button::new("overview-quit")
                                .ghost()
                                .small()
                                .icon(IconName::Power)
                                .tooltip("Quit NeoHtop")
                                .on_click(|_, _, cx| cx.quit()),
                        ),
                ),
        )
    }
}
