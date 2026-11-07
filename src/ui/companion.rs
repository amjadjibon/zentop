//! The menu-bar overview: essential readings and the busiest processes in a
//! small popup anchored to the tray icon. It closes when it loses focus.
use super::monitor::{self, Monitor};
use super::process_view::{format_bytes, identity, Identity};
use super::{theme, Windows, SMALL_ICON};
use crate::native_icons::{Icon, IconRequest};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::progress::Progress;
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

const WIDTH: f32 = 400.;
const HEIGHT: f32 = 600.;
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
        app_id: Some("com.zentop.app".into()),
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
    _clock: gpui_kit::Task<()>,
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
        let clock = cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(1))
                .await;
            if this.update(cx, |_, cx| cx.notify()).is_err() {
                break;
            }
        });
        let mut this = Self {
            _clock: clock,
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
        let busiest = self.busiest(cx);
        // The popup only shows five identities. Holding every past winner's
        // image here would bypass the native icon cache's eviction limit.
        self.icons
            .retain(|id, _| busiest.iter().any(|process| identity(process) == *id));
        let requests: Vec<IconRequest> = busiest
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

fn metric(
    label: &'static str,
    value: Option<f64>,
    detail: String,
    color: gpui_kit::Hsla,
    cx: &App,
) -> impl IntoElement {
    v_flex()
        .flex_1()
        .min_w_0()
        .px_2()
        .gap_2()
        .child(
            h_flex()
                .gap_1p5()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(
                    div()
                        .size_5()
                        .rounded_md()
                        .bg(color.opacity(0.12))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            gpui_kit::component::Icon::new(match label {
                                "CPU" => IconName::Cpu,
                                "Memory" => IconName::MemoryStick,
                                _ => IconName::Gpu,
                            })
                            .xsmall()
                            .text_color(color),
                        ),
                )
                .child(label),
        )
        .child(
            div()
                .text_xl()
                .font_weight(FontWeight::SEMIBOLD)
                .child(value.map_or_else(|| "—".into(), |v| format!("{v:.1}%"))),
        )
        .child(
            Progress::new(label)
                .value(value.unwrap_or(0.) as f32)
                .color(color)
                .h(px(6.)),
        )
        .child(
            div()
                .text_size(px(10.))
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
        let error = monitor.error.clone();
        let busiest = self.busiest(cx);
        let palette = theme::palette(cx);
        let pause_monitor = self.monitor.clone();
        let readings = frame.as_ref().map(|frame| {
            let s = &frame.stats;
            let cpu = (!s.cpu_usage.is_empty()).then(|| {
                s.cpu_usage.iter().map(|v| *v as f64).sum::<f64>() / s.cpu_usage.len() as f64
            });
            let memory =
                (s.memory_total > 0).then(|| s.memory_used as f64 / s.memory_total as f64 * 100.);
            let gpu = frame.gpus.first();
            let rate = network_rate;
            v_flex()
                .gap_3()
                .child(
                    h_flex()
                        .child(metric(
                            "CPU",
                            cpu,
                            format!("{} cores", s.cpu_usage.len()),
                            palette.charts[0],
                            cx,
                        ))
                        .child(metric(
                            "Memory",
                            memory,
                            format!("{} used", format_bytes(s.memory_used)),
                            palette.charts[2],
                            cx,
                        ))
                        .child(metric(
                            "GPU",
                            gpu.and_then(|g| g.utilization),
                            gpu.map_or_else(|| "Unavailable".into(), |g| g.name.clone()),
                            palette.charts[1],
                            cx,
                        )),
                )
                .child(
                    h_flex()
                        .p_3()
                        .gap_3()
                        .rounded_lg()
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().muted.opacity(0.3))
                        .text_xs()
                        .child(
                            v_flex()
                                .flex_1()
                                .gap_1p5()
                                .child(
                                    div()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("Network"),
                                )
                                .child(format!("↓ {}", rate(s.network_rx_bytes)))
                                .child(
                                    div()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!("↑ {}", rate(s.network_tx_bytes))),
                                ),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .gap_1p5()
                                .child(
                                    div()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("Storage free"),
                                )
                                .child(format_bytes(s.disk_free_bytes))
                                .child(
                                    div()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!("of {}", format_bytes(s.disk_total_bytes))),
                                ),
                        ),
                )
        });
        div().size_full().p_2().child(
            v_flex()
                .size_full()
                .rounded_2xl()
                .border_1()
                .border_color(cx.theme().border)
                .bg(palette.card)
                .text_color(cx.theme().foreground)
                .shadow_lg()
                .child(
                    h_flex()
                        .p_4()
                        .gap_2()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(img(SMALL_ICON.clone()).size_7())
                        .child(
                            v_flex()
                                .flex_1()
                                .child(div().font_weight(FontWeight::SEMIBOLD).child("Zentop"))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(overview_status(
                                            paused,
                                            frame.as_ref().map(|f| f.captured_at),
                                            crate::sampling::now_ms(),
                                            monitor.refresh_rate(),
                                        )),
                                ),
                        )
                        .child(
                            Button::new("open-overview-main")
                                .ghost()
                                .small()
                                .icon(IconName::ArrowUpRight)
                                .tooltip("Open Zentop")
                                .on_click(|_, _, cx| cx.defer(super::open_main)),
                        ),
                )
                .child(
                    v_flex()
                        .id("overview-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .p_4()
                        .gap_3()
                        .when_some(error, |this, error| {
                            this.child(div().text_xs().text_color(palette.danger).child(error))
                        })
                        .children(readings)
                        .child(
                            h_flex()
                                .justify_between()
                                .text_xs()
                                .child("Top CPU processes")
                                .child(
                                    div()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("CPU · RAM"),
                                ),
                        )
                        .child(
                            v_flex()
                                .rounded_lg()
                                .border_1()
                                .border_color(cx.theme().border)
                                .children(busiest.into_iter().enumerate().map(|(i, p)| {
                                    let id = identity(&p);
                                    let icon = self.icons.get(&id).cloned().flatten();
                                    h_flex()
                                        .id(("busy", i))
                                        .gap_2p5()
                                        .px_3()
                                        .py_2()
                                        .border_b_1()
                                        .border_color(cx.theme().border)
                                        .cursor_pointer()
                                        .hover(|d| d.bg(cx.theme().accent))
                                        .on_click(move |_, _, cx| {
                                            cx.defer(move |cx| super::show_process(id, cx))
                                        })
                                        .child(match icon {
                                            Some(icon) => img(icon)
                                                .size_5()
                                                .flex_shrink_0()
                                                .into_any_element(),
                                            // Same placeholder as the main list.
                                            None => {
                                                gpui_kit::component::Icon::new(IconName::AppWindow)
                                                    .size_5()
                                                    .flex_shrink_0()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .into_any_element()
                                            }
                                        })
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .truncate()
                                                .text_xs()
                                                .child(p.name),
                                        )
                                        .child(
                                            div().text_xs().child(format!("{:.1}%", p.cpu_usage)),
                                        )
                                        .child(
                                            div()
                                                .w(px(64.))
                                                .text_right()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(format_bytes(p.memory_usage)),
                                        )
                                })),
                        )
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(cx.theme().muted_foreground)
                                .child("Select to inspect. Process CPU can exceed 100%."),
                        ),
                )
                .child(
                    h_flex()
                        .p_3()
                        .gap_2()
                        .justify_between()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .child(
                            Button::new("overview-pause")
                                .outline()
                                .small()
                                .icon(if paused {
                                    IconName::Play
                                } else {
                                    IconName::Pause
                                })
                                .label(if paused { "Resume" } else { "Pause" })
                                .on_click(move |_, _, cx| {
                                    pause_monitor.update(cx, |m, cx| m.set_paused(!paused, cx))
                                }),
                        )
                        .child(
                            Button::new("open-main")
                                .ghost()
                                .small()
                                .label("Open Zentop")
                                .icon(IconName::ArrowUpRight)
                                .on_click(|_, _, cx| cx.defer(super::open_main)),
                        )
                        .child(
                            Button::new("overview-quit")
                                .ghost()
                                .small()
                                .icon(IconName::Power)
                                .tooltip("Quit Zentop")
                                .on_click(|_, _, cx| cx.quit()),
                        ),
                ),
        )
    }
}

fn network_rate(value: Option<u64>) -> String {
    value.map_or_else(|| "—".into(), |b| format!("{}/s", format_bytes(b)))
}

fn overview_status(paused: bool, captured: Option<u64>, now: u64, interval: u64) -> String {
    if paused {
        return "Updates paused".into();
    }
    let Some(captured) = captured else {
        return "Reading this machine…".into();
    };
    let age = now.saturating_sub(captured);
    if age > 10_000.max(interval.saturating_mul(3)) {
        format!("Last reading {}s ago", age / 1000)
    } else {
        format!("Live · updated {}s ago", age / 1000)
    }
}

#[cfg(test)]
mod tests {
    use super::overview_status;
    #[test]
    fn overview_reports_fresh_stale_paused_and_missing_readings() {
        assert_eq!(
            overview_status(false, Some(1000), 4000, 3000),
            "Live · updated 3s ago"
        );
        assert_eq!(
            overview_status(false, Some(1000), 12000, 3000),
            "Last reading 11s ago"
        );
        assert_eq!(
            overview_status(true, Some(1000), 12000, 3000),
            "Updates paused"
        );
        assert_eq!(
            overview_status(false, None, 12000, 3000),
            "Reading this machine…"
        );
        assert_eq!(
            overview_status(false, Some(1000), 12000, 30000),
            "Live · updated 11s ago"
        );
        assert_eq!(
            overview_status(false, Some(9000), 1000, 3000),
            "Live · updated 0s ago"
        );
    }
}
