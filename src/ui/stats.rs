//! Resource cards above the process table.
use super::process_view::{format_bytes, format_uptime, GIB};
use super::resource_details::{self, Resource};
use super::theme::{self, Palette};
use crate::gpu::GpuDevice;
use crate::monitoring::SystemStats;
use crate::sampling::Sample;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Icon, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    canvas, div, point, px, relative, App, Bounds, FontWeight, Hsla, IntoElement, ParentElement,
    PathBuilder, Pixels, SharedString, Styled, Window,
};
use std::rc::Rc;

/// History window shown by the trend lines.
const TREND_MS: u64 = 120_000;
const MIN_CHART_HEIGHT: f32 = 20.;

pub struct Readings<'a> {
    pub stats: &'a SystemStats,
    pub gpus: &'a [GpuDevice],
    pub history: &'a [Sample],
    pub unavailable: bool,
}

pub fn render(r: &Readings, cx: &App) -> impl IntoElement {
    let palette = theme::palette(cx);
    h_flex()
        .gap_3()
        .px_4()
        .py_4()
        .flex_shrink_0()
        .h(px(272.))
        .border_b_1()
        .border_color(cx.theme().border)
        // Proportions of the original layout: CPU and Network get extra room.
        .child(cpu(r, &palette, cx).flex_basis(relative(1.5 / 6.75)))
        .child(memory(r, &palette, cx).flex_basis(relative(1.0 / 6.75)))
        .child(gpu(r, &palette, cx).flex_basis(relative(1.0 / 6.75)))
        .child(storage(r, &palette, cx).flex_basis(relative(1.0 / 6.75)))
        .child(system(r, cx).flex_basis(relative(1.05 / 6.75)))
        .child(network(r, &palette, cx).flex_basis(relative(1.2 / 6.75)))
}

fn card(
    icon: IconName,
    title: &'static str,
    accent: Hsla,
    badge: Option<String>,
    resource: Resource,
    cx: &App,
) -> gpui_kit::Div {
    v_flex()
        .flex_1()
        .min_w_0()
        .h_full()
        .gap_1()
        .p_4()
        .rounded(cx.theme().radius_lg)
        .border_1()
        .border_color(cx.theme().border)
        .bg(theme::palette(cx).card)
        .child(
            h_flex()
                .justify_between()
                .mb_2()
                .child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            div()
                                .size_5()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_md()
                                .bg(accent.opacity(0.12))
                                .child(Icon::new(icon).xsmall().text_color(accent)),
                        )
                        .child(title),
                )
                .child(
                    h_flex()
                        .gap_1()
                        .when_some(badge, |this, badge| {
                            this.child(
                                div()
                                    .px_1p5()
                                    .rounded_md()
                                    .text_xs()
                                    .bg(cx.theme().secondary)
                                    .text_color(cx.theme().muted_foreground)
                                    .child(badge),
                            )
                        })
                        .child(
                            Button::new(SharedString::from(format!("details-{title}")))
                                .ghost()
                                .xsmall()
                                .icon(IconName::ArrowUpRight)
                                .tooltip(format!("{title} details"))
                                .on_click(move |_, window, cx| {
                                    resource_details::open(resource, window, cx)
                                }),
                        ),
                ),
        )
}

/// A large reading with an optional smaller unit, and a caption below it.
fn headline(
    value: Option<String>,
    unit: &'static str,
    caption: impl Into<SharedString>,
    cx: &App,
) -> impl IntoElement {
    let caption = caption.into();
    let size = if caption == "Average utilization" {
        30.
    } else if caption == "Uptime" {
        20.
    } else {
        24.
    };
    let unit = if value.is_some() { unit } else { "" };
    v_flex()
        .child(
            h_flex()
                .items_baseline()
                .gap_0p5()
                .child(
                    div()
                        .text_size(px(size))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(value.unwrap_or_else(|| "—".into())),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(unit),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(caption),
        )
}

fn line(label: impl Into<SharedString>, value: String, cx: &App) -> impl IntoElement {
    h_flex()
        .justify_between()
        .text_xs()
        .child(
            div()
                .text_color(cx.theme().muted_foreground)
                .child(label.into()),
        )
        .child(value)
}

fn caption(text: String, cx: &App) -> impl IntoElement {
    div()
        .text_size(px(10.))
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

fn percent(used: u64, total: u64) -> Option<f64> {
    (total > 0).then(|| (used as f64 / total as f64 * 100.0).clamp(0.0, 100.0))
}

/// A chart filling the rest of a card, with its scale caption underneath.
fn chart_block(chart: impl IntoElement, scale: String, cx: &App) -> impl IntoElement {
    v_flex()
        .flex_1()
        .min_h(px(MIN_CHART_HEIGHT))
        .justify_end()
        .gap_1()
        .pt_1()
        .child(
            div()
                .h(px(48.))
                .min_h(px(MIN_CHART_HEIGHT))
                .w_full()
                .flex()
                .child(chart),
        )
        .child(caption(scale, cx))
}

fn cpu(r: &Readings, palette: &Palette, cx: &App) -> gpui_kit::Div {
    let cores = &r.stats.cpu_usage;
    let average = (!cores.is_empty() && cores.iter().all(|v| v.is_finite()))
        .then(|| cores.iter().map(|v| *v as f64).sum::<f64>() / cores.len() as f64);
    let columns = match cores.len() {
        0..=6 => cores.len().max(1),
        7..=12 => cores.len().div_ceil(2),
        _ => 8,
    };
    let accent = palette.charts[0];
    card(
        IconName::Cpu,
        "CPU",
        accent,
        Some(format!("{} cores", cores.len())),
        Resource::Cpu,
        cx,
    )
    .child(headline(
        average.map(|v| format!("{v:.1}")),
        "%",
        "Average utilization",
        cx,
    ))
    .child(
        div()
            .mt_1()
            .grid()
            .grid_cols(columns as u16)
            .gap_x_1()
            .gap_y_1()
            .max_h(px(52.))
            .overflow_hidden()
            .children(cores.iter().enumerate().map(|(i, value)| {
                v_flex()
                    .gap_0p5()
                    .items_center()
                    .child(
                        Progress::new(("core", i))
                            .value(if value.is_finite() {
                                value.clamp(0.0, 100.0)
                            } else {
                                0.0
                            })
                            .color(accent)
                            .h(px(3.)),
                    )
                    .child(
                        div()
                            .text_size(px(9.))
                            .text_color(cx.theme().muted_foreground)
                            .child(i.to_string()),
                    )
            })),
    )
    .child(chart_block(
        trend(r.history, |s| s.cpu, accent, r.unavailable),
        "2m · 0–100%".into(),
        cx,
    ))
}

fn memory(r: &Readings, palette: &Palette, cx: &App) -> gpui_kit::Div {
    let s = r.stats;
    let accent = palette.charts[2];
    card(
        IconName::MemoryStick,
        "Memory",
        accent,
        None,
        Resource::Memory,
        cx,
    )
    .child(headline(
        Some(format!("{:.1} GB", s.memory_used as f64 / GIB)),
        "",
        format!("used of {:.1} GB", s.memory_total as f64 / GIB),
        cx,
    ))
    .child(div().mt_1())
    .child(line("Not in use", format_bytes(s.memory_free), cx))
    .child(line(
        "Utilization",
        percent(s.memory_used, s.memory_total).map_or_else(|| "—".into(), |v| format!("{v:.1}%")),
        cx,
    ))
    .child(chart_block(
        trend(r.history, |s| s.memory, accent, r.unavailable),
        "2m · 0–100%".into(),
        cx,
    ))
}

fn gpu(r: &Readings, palette: &Palette, cx: &App) -> gpui_kit::Div {
    let accent = palette.charts[1];
    let Some(device) = r.gpus.first() else {
        return card(IconName::Gpu, "GPU", accent, None, Resource::Gpu, cx).child(headline(
            None,
            "%",
            "No supported GPU detected",
            cx,
        ));
    };
    let id = device.id.clone();
    let extra = (r.gpus.len() > 1).then(|| format!("+{} more", r.gpus.len() - 1));
    let memory_kind = match device.memory_kind.as_str() {
        "shared" => "Shared",
        "dedicated" => "Dedicated",
        _ => "Memory",
    };
    card(IconName::Gpu, "GPU", accent, extra, Resource::Gpu, cx)
        .child(headline(
            device.utilization.map(|v| format!("{v:.1}")),
            "%",
            device.name.clone(),
            cx,
        ))
        .when_some(device.memory_used, |this, used| {
            this.child(
                div()
                    .mt_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{memory_kind} · {}", format_bytes(used))),
            )
        })
        .child(chart_block(
            trend(
                r.history,
                move |s| s.gpus.get(&id).copied().flatten(),
                accent,
                r.unavailable,
            ),
            "2m · 0–100%".into(),
            cx,
        ))
}

fn storage(r: &Readings, palette: &Palette, cx: &App) -> gpui_kit::Div {
    let s = r.stats;
    let accent = palette.charts[3];
    let used = percent(s.disk_used_bytes, s.disk_total_bytes);
    let color = match used {
        Some(v) if v >= 95.0 => palette.danger,
        Some(v) if v >= 90.0 => palette.warning,
        _ => accent,
    };
    card(
        IconName::HardDrive,
        "Storage",
        accent,
        None,
        Resource::Storage,
        cx,
    )
    .child(headline(
        Some(format_bytes(s.disk_free_bytes)),
        "",
        format!("free of {}", format_bytes(s.disk_total_bytes)),
        cx,
    ))
    .child(div().mt_1())
    .child(line("Used", format_bytes(s.disk_used_bytes), cx))
    .child(
        v_flex()
            .mt_auto()
            .gap_1p5()
            .child(
                Progress::new("disk")
                    .value(used.unwrap_or(0.0) as f32)
                    .color(color)
                    .h(px(8.)),
            )
            .child(
                h_flex()
                    .justify_between()
                    .text_size(px(10.))
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        h_flex()
                            .gap_1()
                            .child(div().size_1p5().rounded_full().bg(color))
                            .child(used.map_or_else(|| "—".into(), |v| format!("{v:.1}% used"))),
                    )
                    .child(used.map_or_else(String::new, |v| format!("{:.1}% free", 100.0 - v))),
            ),
    )
}

fn system(r: &Readings, cx: &App) -> gpui_kit::Div {
    let s = r.stats;
    let accent = theme::palette(cx).charts[4];
    let load = |label: &'static str, value: f64| {
        v_flex()
            .flex_1()
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .child(
                div()
                    .text_base()
                    .font_weight(FontWeight::MEDIUM)
                    .child(format!("{value:.2}")),
            )
    };
    card(
        IconName::Server,
        "System",
        accent,
        None,
        Resource::System,
        cx,
    )
    .child(headline(Some(format_uptime(s.uptime)), "", "Uptime", cx))
    .child(
        v_flex()
            .mt_auto()
            .pt_2()
            .gap_1()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Load average"),
            )
            .child(
                h_flex()
                    .child(load("1 min", s.load_avg[0]))
                    .child(load("5 min", s.load_avg[1]))
                    .child(load("15 min", s.load_avg[2])),
            ),
    )
}

/// Splits a per-second byte rate into a number and a unit, e.g. ("22.8", "MB/s").
fn rate_parts(bytes_per_second: Option<u64>) -> (Option<String>, &'static str) {
    const UNITS: [&str; 5] = ["B/s", "KB/s", "MB/s", "GB/s", "TB/s"];
    let Some(bytes) = bytes_per_second else {
        return (None, "");
    };
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    (Some(format!("{value:.1}")), UNITS[unit])
}

fn legend(color: Hsla, dashed: bool, label: &'static str, cx: &App) -> impl IntoElement {
    h_flex()
        .gap_1()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(
            h_flex()
                .gap(px(2.))
                .when(dashed, |this| {
                    this.child(div().w(px(4.)).h(px(2.)).bg(color))
                        .child(div().w(px(4.)).h(px(2.)).bg(color))
                })
                .when(!dashed, |this| {
                    this.child(div().w(px(10.)).h(px(2.)).bg(color))
                }),
        )
        .child(label)
}

fn network(r: &Readings, palette: &Palette, cx: &App) -> gpui_kit::Div {
    let s = r.stats;
    let (received, sent) = (palette.charts[0], palette.charts[3]);
    let (value, unit) = rate_parts(s.network_rx_bytes);
    let peak = r
        .history
        .iter()
        .flat_map(|s| [s.received, s.sent])
        .flatten()
        .max()
        .unwrap_or(0)
        .max(1);
    let scale = peak as f64;
    card(
        IconName::Network,
        "Network",
        received,
        None,
        Resource::Network,
        cx,
    )
    .child(
        h_flex()
            .items_baseline()
            .gap_0p5()
            .child(
                div()
                    .text_2xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(value.unwrap_or_else(|| "—".into())),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(unit),
            ),
    )
    .child(legend(received, false, "↓ Received", cx))
    .child(
        h_flex()
            .justify_between()
            .text_xs()
            .child(legend(sent, true, "↑ Sent", cx))
            .child(
                s.network_tx_bytes
                    .map_or_else(|| "—".into(), |b| format!("{}/s", format_bytes(b))),
            ),
    )
    .child(chart_block(
        trends(
            r.history,
            vec![
                Series {
                    value: Rc::new(move |s: &Sample| s.received.map(|v| v as f64 / scale * 100.0)),
                    color: received,
                    fill: true,
                },
                Series {
                    value: Rc::new(move |s: &Sample| s.sent.map(|v| v as f64 / scale * 100.0)),
                    color: sent,
                    fill: false,
                },
            ],
            r.unavailable,
        ),
        format!("2m · 0–{}/s", format_bytes(peak)),
        cx,
    ))
}

/// Reads one 0–100 value from a sample, or `None` where it is missing.
type Reading = Rc<dyn Fn(&Sample) -> Option<f64>>;
/// A resolved line: (timestamp, value) points, color, and whether it is filled.
type Line = (Vec<(u64, Option<f64>)>, Hsla, bool);

/// One line on a trend chart, as a 0–100 value per sample.
struct Series {
    value: Reading,
    color: Hsla,
    fill: bool,
}

/// A filled 0–100 trend over the last two minutes.
pub fn trend(
    history: &[Sample],
    value: impl Fn(&Sample) -> Option<f64> + 'static,
    color: Hsla,
    unavailable: bool,
) -> impl IntoElement {
    trends(
        history,
        vec![Series {
            value: Rc::new(value),
            color,
            fill: true,
        }],
        unavailable,
    )
}

/// Trend lines over the last two minutes, stretched to the history available.
/// Missing readings break a line instead of being drawn as zero.
fn trends(history: &[Sample], series: Vec<Series>, unavailable: bool) -> impl IntoElement {
    let end = history.last().map_or(0, |s| s.timestamp);
    let start = end.saturating_sub(TREND_MS);
    let lines: Vec<Line> = series
        .into_iter()
        .map(|s| {
            let points = history
                .iter()
                .filter(|h| h.timestamp >= start)
                .map(|h| {
                    (
                        h.timestamp,
                        (s.value)(h)
                            .filter(|v| v.is_finite())
                            .map(|v| v.clamp(0.0, 100.0)),
                    )
                })
                .collect();
            (points, s.color, s.fill)
        })
        .collect();
    let opacity = if unavailable { 0.4 } else { 1.0 };
    canvas(
        move |_, _, _| lines,
        move |bounds: Bounds<Pixels>, lines, window: &mut Window, _| {
            let span = end.saturating_sub(start).max(1) as f64;
            let to_point = |t: u64, v: f64| {
                point(
                    bounds.origin.x + bounds.size.width * ((t - start) as f64 / span) as f32,
                    bounds.origin.y + bounds.size.height * (1.0 - v as f32 / 100.0),
                )
            };
            let bottom = bounds.origin.y + bounds.size.height;
            for (points, color, fill) in lines {
                for run in points.split(|(_, v)| v.is_none()).filter(|r| !r.is_empty()) {
                    let run: Vec<_> = run
                        .iter()
                        .map(|(t, v)| to_point(*t, v.unwrap_or(0.0)))
                        .collect();
                    if fill && run.len() > 1 {
                        let mut area = PathBuilder::fill();
                        area.move_to(point(run[0].x, bottom));
                        for p in &run {
                            area.line_to(*p);
                        }
                        area.line_to(point(run[run.len() - 1].x, bottom));
                        area.close();
                        if let Ok(path) = area.build() {
                            window.paint_path(path, color.opacity(0.18 * opacity));
                        }
                    }
                    let mut stroke = PathBuilder::stroke(px(1.5));
                    stroke.move_to(run[0]);
                    for p in &run[1..] {
                        stroke.line_to(*p);
                    }
                    if run.len() == 1 {
                        stroke.line_to(point(run[0].x + px(1.), run[0].y));
                    }
                    if let Ok(path) = stroke.build() {
                        window.paint_path(path, color.opacity(opacity));
                    }
                }
            }
        },
    )
    .size_full()
}

/// Shared receive/send chart for the network detail sheet.
pub(super) fn network_trend(
    history: &[Sample],
    palette: &Palette,
    unavailable: bool,
) -> impl IntoElement {
    let scale = history
        .iter()
        .flat_map(|s| [s.received, s.sent])
        .flatten()
        .max()
        .unwrap_or(1)
        .max(1) as f64;
    trends(
        history,
        vec![
            Series {
                value: Rc::new(move |s| s.received.map(|v| v as f64 / scale * 100.)),
                color: palette.charts[0],
                fill: true,
            },
            Series {
                value: Rc::new(move |s| s.sent.map(|v| v as f64 / scale * 100.)),
                color: palette.charts[3],
                fill: false,
            },
        ],
        unavailable,
    )
}
