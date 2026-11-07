//! Resource cards above the process table.
use super::process_view::{format_bytes, format_uptime, GIB};
use super::theme::{self, Palette};
use crate::gpu::GpuDevice;
use crate::monitoring::SystemStats;
use crate::sampling::Sample;
use gpui_kit::assets::IconName;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Icon, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    canvas, div, point, px, AnyElement, App, Bounds, FontWeight, Hsla, IntoElement, ParentElement,
    PathBuilder, Pixels, SharedString, Styled, Window,
};

/// History window shown by the trend lines.
const TREND_MS: u64 = 120_000;

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
        .pt_3()
        .flex_shrink_0()
        .h(px(236.))
        .child(cpu(r, &palette, cx))
        .child(memory(r, &palette, cx))
        .child(gpu(r, &palette, cx))
        .child(storage(r, &palette, cx))
        .child(system(r, cx))
        .child(network(r, &palette, cx))
}

fn card(
    icon: IconName,
    title: &'static str,
    accent: Hsla,
    badge: Option<String>,
    cx: &App,
) -> gpui_kit::Div {
    v_flex()
        .flex_1()
        .min_w_0()
        .h_full()
        .gap_2()
        .p_3()
        .rounded(cx.theme().radius_lg)
        .border_1()
        .border_color(cx.theme().border)
        .bg(theme::palette(cx).card)
        .child(
            h_flex()
                .justify_between()
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
                }),
        )
}

fn headline(
    value: Option<String>,
    unit: &'static str,
    caption: impl Into<SharedString>,
    cx: &App,
) -> impl IntoElement {
    let unit = if value.is_some() { unit } else { "" };
    v_flex()
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
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(caption.into()),
        )
}

fn line(label: &'static str, value: String, cx: &App) -> impl IntoElement {
    h_flex()
        .justify_between()
        .text_xs()
        .child(div().text_color(cx.theme().muted_foreground).child(label))
        .child(
            div()
                .font_family(cx.theme().mono_font_family.clone())
                .child(value),
        )
}

fn percent(used: u64, total: u64) -> Option<f64> {
    (total > 0).then(|| (used as f64 / total as f64 * 100.0).clamp(0.0, 100.0))
}

fn cpu(r: &Readings, palette: &Palette, cx: &App) -> AnyElement {
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
            .grid()
            .grid_cols(columns as u16)
            .gap_x_0p5()
            .gap_y_1()
            .max_h(px(40.))
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
                            .h(px(4.)),
                    )
                    .child(
                        div()
                            .text_size(px(9.))
                            .text_color(cx.theme().muted_foreground)
                            .child(i.to_string()),
                    )
            })),
    )
    .child(trend(r.history, |s| s.cpu, accent, r.unavailable))
    .into_any_element()
}

fn memory(r: &Readings, palette: &Palette, cx: &App) -> AnyElement {
    let s = r.stats;
    let accent = palette.charts[1];
    card(
        IconName::MemoryStick,
        "Memory",
        accent,
        Some(format_bytes(s.memory_total)),
        cx,
    )
    .child(headline(
        percent(s.memory_used, s.memory_total).map(|v| format!("{v:.1}")),
        "%",
        format!(
            "{:.1} of {:.1} GB used",
            s.memory_used as f64 / GIB,
            s.memory_total as f64 / GIB
        ),
        cx,
    ))
    .child(line("Available", format_bytes(s.memory_free), cx))
    .child(line("Cached", format_bytes(s.memory_cached), cx))
    .child(trend(r.history, |s| s.memory, accent, r.unavailable))
    .into_any_element()
}

fn gpu(r: &Readings, palette: &Palette, cx: &App) -> AnyElement {
    let accent = palette.charts[3];
    let Some(device) = r.gpus.first() else {
        return card(IconName::Gpu, "GPU", accent, None, cx)
            .child(headline(None, "%", "No GPU readings on this system", cx))
            .into_any_element();
    };
    let id = device.id.clone();
    let extra = (r.gpus.len() > 1).then(|| format!("+{} more", r.gpus.len() - 1));
    card(IconName::Gpu, "GPU", accent, extra, cx)
        .child(headline(
            device.utilization.map(|v| format!("{v:.1}")),
            "%",
            device.name.clone(),
            cx,
        ))
        .when_some(
            device.memory_used.zip(device.memory_total),
            |this, (used, total)| {
                this.child(line(
                    if device.memory_kind == "shared" {
                        "Shared memory"
                    } else {
                        "Memory"
                    },
                    format!("{} / {}", format_bytes(used), format_bytes(total)),
                    cx,
                ))
            },
        )
        .when_some(device.temperature, |this, t| {
            this.child(line("Temperature", format!("{t:.0} °C"), cx))
        })
        .when_some(device.power_watts, |this, w| {
            this.child(line("Power", format!("{w:.1} W"), cx))
        })
        .child(trend(
            r.history,
            move |s| s.gpus.get(&id).copied().flatten(),
            accent,
            r.unavailable,
        ))
        .into_any_element()
}

fn storage(r: &Readings, palette: &Palette, cx: &App) -> AnyElement {
    let s = r.stats;
    let accent = palette.charts[2];
    let used = percent(s.disk_used_bytes, s.disk_total_bytes);
    let color = match used {
        Some(v) if v >= 90.0 => palette.danger,
        Some(v) if v >= 75.0 => palette.warning,
        _ => accent,
    };
    card(IconName::HardDrive, "Storage", accent, None, cx)
        .child(headline(
            used.map(|v| format!("{v:.1}")),
            "%",
            "Used across local volumes",
            cx,
        ))
        .child(
            Progress::new("disk")
                .value(used.unwrap_or(0.0) as f32)
                .color(color)
                .h(px(6.)),
        )
        .child(line("Used", format_bytes(s.disk_used_bytes), cx))
        .child(line("Free", format_bytes(s.disk_free_bytes), cx))
        .child(line("Total", format_bytes(s.disk_total_bytes), cx))
        .into_any_element()
}

fn system(r: &Readings, cx: &App) -> AnyElement {
    let s = r.stats;
    let identity = &s.details.identity;
    let accent = theme::palette(cx).charts[4];
    card(IconName::Monitor, "System", accent, None, cx)
        .child(headline(Some(format_uptime(s.uptime)), "", "Uptime", cx))
        .child(line(
            "Load",
            format!(
                "{:.2} {:.2} {:.2}",
                s.load_avg[0], s.load_avg[1], s.load_avg[2]
            ),
            cx,
        ))
        .child(line(
            "OS",
            identity
                .os_version
                .clone()
                .unwrap_or_else(|| identity.platform.clone()),
            cx,
        ))
        .child(line("Arch", identity.architecture.clone(), cx))
        .into_any_element()
}

/// Network readings are already per-second rates.
fn rate(bytes_per_second: Option<u64>) -> String {
    bytes_per_second.map_or_else(|| "—".into(), |b| format!("{}/s", format_bytes(b)))
}

fn network(r: &Readings, palette: &Palette, cx: &App) -> AnyElement {
    let s = r.stats;
    let accent = palette.charts[4];
    let peak = r
        .history
        .iter()
        .flat_map(|s| [s.received, s.sent])
        .flatten()
        .max()
        .unwrap_or(0)
        .max(1) as f64;
    card(IconName::Network, "Network", accent, None, cx)
        .child(headline(Some(rate(s.network_rx_bytes)), "", "Download", cx))
        .child(line("Upload", rate(s.network_tx_bytes), cx))
        .child(trend(
            r.history,
            move |s| s.received.map(|v| v as f64 / peak * 100.0),
            accent,
            r.unavailable,
        ))
        .into_any_element()
}

/// A 0–100 trend over the last two minutes. Missing readings break the line
/// instead of being drawn as zero.
fn trend(
    history: &[Sample],
    value: impl Fn(&Sample) -> Option<f64>,
    color: Hsla,
    unavailable: bool,
) -> impl IntoElement {
    let end = history.last().map_or(0, |s| s.timestamp);
    let points: Vec<(u64, Option<f64>)> = history
        .iter()
        .filter(|s| s.timestamp + TREND_MS >= end)
        .map(|s| {
            (
                s.timestamp,
                value(s)
                    .filter(|v| v.is_finite())
                    .map(|v| v.clamp(0.0, 100.0)),
            )
        })
        .collect();
    let opacity = if unavailable { 0.4 } else { 1.0 };
    div().flex_1().min_h(px(24.)).w_full().child(
        canvas(
            move |_, _, _| points,
            move |bounds: Bounds<Pixels>, points, window: &mut Window, _| {
                let start = end.saturating_sub(TREND_MS) as f64;
                let to_point = |t: u64, v: f64| {
                    point(
                        bounds.origin.x
                            + bounds.size.width * ((t as f64 - start) / TREND_MS as f64) as f32,
                        bounds.origin.y + bounds.size.height * (1.0 - v as f32 / 100.0),
                    )
                };
                for run in points.split(|(_, v)| v.is_none()).filter(|r| !r.is_empty()) {
                    let run: Vec<_> = run
                        .iter()
                        .map(|(t, v)| to_point(*t, v.unwrap_or(0.0)))
                        .collect();
                    let bottom = bounds.origin.y + bounds.size.height;
                    if run.len() > 1 {
                        let mut area = PathBuilder::fill();
                        area.move_to(point(run[0].x, bottom));
                        for p in &run {
                            area.line_to(*p);
                        }
                        area.line_to(point(run[run.len() - 1].x, bottom));
                        area.close();
                        if let Ok(path) = area.build() {
                            window.paint_path(path, color.opacity(0.15 * opacity));
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
            },
        )
        .size_full(),
    )
}
