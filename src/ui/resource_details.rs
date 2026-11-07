//! Detail sheets opened from each resource card. They read the shared model on
//! every render, so readings stay live while a sheet is open.
use super::monitor;
use super::process_view::{format_bytes, format_uptime};
use super::stats;
use super::theme;
use crate::sampling::Sample;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    div, px, AnyElement, App, FontWeight, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Resource {
    Cpu,
    Memory,
    Gpu,
    Storage,
    System,
    Network,
}

impl Resource {
    fn title(self) -> &'static str {
        match self {
            Resource::Cpu => "CPU",
            Resource::Memory => "Memory",
            Resource::Gpu => "Graphics",
            Resource::Storage => "Storage",
            Resource::System => "System",
            Resource::Network => "Network",
        }
    }
}

pub fn open(resource: Resource, window: &mut Window, cx: &mut App) {
    window.open_sheet(cx, move |sheet, _, cx| {
        let monitor = monitor::global(cx).read(cx);
        let body = match (&monitor.frame, monitor.error.is_some()) {
            (Some(frame), unavailable) => {
                let frame = frame.clone();
                let history = monitor.history.clone();
                content(resource, &frame, &history, unavailable, cx)
            }
            (None, _) => note("Waiting for the first reading…", cx).into_any_element(),
        };
        sheet
            .size(px(480.))
            .title(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(resource.title()),
            )
            .child(
                v_flex()
                    .id("resource-details")
                    .size_full()
                    .overflow_y_scroll()
                    .gap_3()
                    .child(body),
            )
    });
}

fn rows(items: Vec<(&'static str, String)>, cx: &App) -> impl IntoElement {
    v_flex().children(items.into_iter().map(|(label, value)| {
        h_flex()
            .justify_between()
            .gap_4()
            .py_1p5()
            .border_b_1()
            .border_color(cx.theme().border.opacity(0.5))
            .text_sm()
            .child(div().text_color(cx.theme().muted_foreground).child(label))
            .child(div().text_right().child(value))
    }))
}

fn note(text: &'static str, cx: &App) -> impl IntoElement {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

fn heading(text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .pt_2()
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .child(text.into())
}

fn chart(
    history: &[Sample],
    value: impl Fn(&Sample) -> Option<f64> + 'static,
    color: gpui_kit::Hsla,
    unavailable: bool,
) -> impl IntoElement {
    div()
        .h(px(96.))
        .flex()
        .child(stats::trend(history, value, color, unavailable))
}

fn unavailable_or<T>(value: Option<T>, f: impl FnOnce(T) -> String) -> String {
    value.map_or_else(|| "Unavailable".into(), f)
}

fn content(
    resource: Resource,
    frame: &crate::sampling::Frame,
    history: &[Sample],
    unavailable: bool,
    cx: &App,
) -> AnyElement {
    let s = &frame.stats;
    let identity = &s.details.identity;
    let palette = theme::palette(cx);
    match resource {
        Resource::Cpu => {
            let average = (!s.cpu_usage.is_empty())
                .then(|| s.cpu_usage.iter().map(|v| *v as f64).sum::<f64>() / s.cpu_usage.len() as f64);
            v_flex()
                .gap_2()
                .child(heading(identity.cpu_model.clone().unwrap_or_else(|| "Processor".into())))
                .child(rows(
                    vec![
                        ("Average utilization", unavailable_or(average, |v| format!("{v:.1}%"))),
                        ("Physical cores", unavailable_or(identity.physical_cores, |v| v.to_string())),
                        ("Logical processors", s.cpu_usage.len().to_string()),
                    ],
                    cx,
                ))
                .child(heading("Utilization over time"))
                .child(chart(history, |s| s.cpu, palette.charts[0], unavailable))
                .child(heading("Per-core activity"))
                .child(v_flex().gap_1().children(s.cpu_usage.iter().enumerate().map(|(i, v)| {
                    h_flex()
                        .gap_2()
                        .text_xs()
                        .child(div().w(px(56.)).text_color(cx.theme().muted_foreground).child(format!("Core {i}")))
                        .child(div().flex_1().child(Progress::new(("core-detail", i)).value(v.clamp(0.0, 100.0)).color(palette.charts[0])))
                        .child(div().w(px(48.)).text_right().child(format!("{v:.1}%")))
                })))
                .child(note("The overview averages utilization across logical processors. A process can exceed 100% CPU when it uses several cores.", cx))
                .into_any_element()
        }
        Resource::Memory => v_flex()
            .gap_2()
            .child(chart(history, |s| s.memory, palette.charts[1], unavailable))
            .child(rows(
                vec![
                    ("Physical memory", format_bytes(s.memory_total)),
                    ("In use", format_bytes(s.memory_used)),
                    ("Not in use", format_bytes(s.memory_free)),
                    ("Unallocated", format_bytes(s.details.memory_unallocated)),
                    ("Cached", format_bytes(s.memory_cached)),
                    (
                        "Swap",
                        if s.details.swap_total > 0 {
                            format!("{} of {}", format_bytes(s.details.swap_used), format_bytes(s.details.swap_total))
                        } else {
                            "Not configured".into()
                        },
                    ),
                ],
                cx,
            ))
            .child(note("Not in use is total RAM minus reported use. Unallocated is the OS free-page count. These can overlap, and swap use alone does not measure memory pressure.", cx))
            .into_any_element(),
        Resource::Gpu => {
            if frame.gpus.is_empty() {
                return note("No graphics adapters reported readings on this system.", cx).into_any_element();
            }
            v_flex()
                .gap_2()
                .children(frame.gpus.iter().map(|g| {
                    let id = g.id.clone();
                    v_flex()
                        .gap_1()
                        .child(heading(format!("{} · {}", g.name, g.vendor)))
                        .child(chart(history, move |s| s.gpus.get(&id).copied().flatten(), palette.charts[3], unavailable))
                        .child(rows(
                            vec![
                                ("Utilization", unavailable_or(g.utilization, |v| format!("{v:.1}%"))),
                                (
                                    "Memory",
                                    match (g.memory_used, g.memory_total) {
                                        (Some(u), Some(t)) => format!("{} of {} ({})", format_bytes(u), format_bytes(t), g.memory_kind),
                                        _ => "Unavailable".into(),
                                    },
                                ),
                                ("Temperature", unavailable_or(g.temperature, |v| format!("{v:.0} °C"))),
                                ("Power", unavailable_or(g.power_watts, |v| format!("{v:.1} W"))),
                                ("Core clock", unavailable_or(g.core_clock_mhz, |v| format!("{v:.0} MHz"))),
                                ("Cores", unavailable_or(g.cores, |v| v.to_string())),
                                ("Source", g.source.clone()),
                            ],
                            cx,
                        ))
                }))
                .child(note("Readings depend on the driver. Missing counters never mean idle hardware. Apple silicon shared GPU memory is part of system RAM.", cx))
                .into_any_element()
        }
        Resource::Storage => {
            let volumes = &s.details.volumes;
            v_flex()
                .gap_2()
                .when(volumes.is_empty(), |this| this.child(note("No volumes reported.", cx)))
                .children(volumes.iter().enumerate().map(|(i, v)| {
                    let used = v.total.saturating_sub(v.available);
                    let percent = if v.total > 0 { used as f32 / v.total as f32 * 100.0 } else { 0.0 };
                    v_flex()
                        .gap_1()
                        .child(heading(format!("{} · {}", v.mount, if v.name.is_empty() { "Volume" } else { &v.name })))
                        .child(Progress::new(("volume", i)).value(percent).color(palette.charts[2]))
                        .child(rows(
                            vec![
                                ("Capacity", if v.total > 0 { format_bytes(v.total) } else { "Unavailable".into() }),
                                ("Used", if v.total > 0 { format_bytes(used) } else { "Unavailable".into() }),
                                ("Available", if v.total > 0 { format_bytes(v.available) } else { "Unavailable".into() }),
                                ("File system", if v.file_system.is_empty() { "Unavailable".into() } else { v.file_system.clone() }),
                                ("Access", if v.read_only { "Read only".into() } else { "Read and write".into() }),
                            ],
                            cx,
                        ))
                }))
                .child(note("These are file-system capacity readings, not a scan of files. Volumes can share one storage pool (APFS), so their capacities can overlap.", cx))
                .into_any_element()
        }
        Resource::System => v_flex()
            .gap_2()
            .child(heading(identity.os_version.clone().unwrap_or_else(|| "Operating system unavailable".into())))
            .child(rows(
                vec![
                    ("Uptime", format_uptime(s.uptime)),
                    ("Kernel", identity.kernel_version.clone().unwrap_or_else(|| "Unavailable".into())),
                    ("Architecture", identity.architecture.clone()),
                    ("Processor", identity.cpu_model.clone().unwrap_or_else(|| "Unavailable".into())),
                    ("Logical processors", s.cpu_usage.len().to_string()),
                    ("Load average (1, 5, 15 min)", format!("{:.2}  {:.2}  {:.2}", s.load_avg[0], s.load_avg[1], s.load_avg[2])),
                ],
                cx,
            ))
            .child(note("Load is not a CPU percentage. Uptime is the time since the operating system started.", cx))
            .into_any_element(),
        Resource::Network => {
            let rate = |v: Option<u64>| unavailable_or(v, |b| format!("{}/s", format_bytes(b)));
            let mut interfaces = s.details.interfaces.clone();
            // Interfaces with traffic first, then by name.
            interfaces.sort_by(|a, b| {
                (b.received_total + b.sent_total > 0)
                    .cmp(&(a.received_total + a.sent_total > 0))
                    .then_with(|| a.name.cmp(&b.name))
            });
            v_flex()
                .gap_2()
                .child(rows(vec![("Received", rate(s.network_rx_bytes)), ("Sent", rate(s.network_tx_bytes))], cx))
                .child(heading("Interfaces"))
                .when(interfaces.is_empty(), |this| this.child(note("No network interfaces were reported.", cx)))
                .children(interfaces.into_iter().map(|i| {
                    v_flex()
                        .py_1p5()
                        .border_b_1()
                        .border_color(cx.theme().border.opacity(0.5))
                        .child(div().text_sm().child(i.name.clone()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!(
                                    "↓ {} · ↑ {} · totals {} / {}",
                                    rate(i.received),
                                    rate(i.sent),
                                    format_bytes(i.received_total),
                                    format_bytes(i.sent_total)
                                )),
                        )
                }))
                .child(note("Rates use the shared sampling interval. Totals can reset when an interface restarts, and virtual interfaces can count the same traffic twice.", cx))
                .into_any_element()
        }
    }
}
