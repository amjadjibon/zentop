//! Detail sheets opened from each resource card. They read the shared model on
//! every render, so readings stay live while a sheet is open.
use super::monitor;
use super::process_view::{format_bytes, format_uptime};
use super::stats;
use super::theme;
use crate::sampling::Sample;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::progress::Progress;
use gpui_kit::AppContext;
use std::{cell::RefCell, rc::Rc};

#[derive(Default)]
struct Selection {
    volume: String,
    gpu: String,
    unused: bool,
    interface_order: Vec<String>,
}

use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Sizable, WindowExt};
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
    let selection = Rc::new(RefCell::new(Selection::default()));
    let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search all interfaces…"));
    let subscription = cx.subscribe(&search, |_, event: &InputEvent, cx| {
        if matches!(event, InputEvent::Change) {
            cx.refresh_windows();
        }
    });
    window.open_sheet(cx, move |sheet, _, cx| {
        let _keep_subscription = &subscription;
        let monitor = monitor::global(cx).read(cx);
        let body = match (&monitor.frame, monitor.error.is_some()) {
            (Some(frame), unavailable) => {
                let frame = frame.clone();
                let history = monitor.history.clone();
                content(
                    resource,
                    &frame,
                    &history,
                    unavailable,
                    (&selection, &search),
                    cx,
                )
            }
            (None, _) => note("Waiting for the first reading…", cx).into_any_element(),
        };
        sheet
            .size(px(448.))
            .title(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(resource.title())
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(match resource {
                                Resource::Cpu => "Overall utilization and activity on each core.",
                                Resource::Memory => "Physical RAM, allocation, and swap.",
                                Resource::Gpu => "Driver-reported readings from this machine.",
                                Resource::Storage => {
                                    "Mounted volumes and their available capacity."
                                }
                                Resource::System => "Operating system, uptime, and system load.",
                                Resource::Network => {
                                    "Traffic across this machine’s network interfaces."
                                }
                            }),
                    )
                    .child(div().text_xs().child(if monitor.error.is_some() {
                        "Updates unavailable"
                    } else if monitor.paused {
                        "Paused"
                    } else {
                        "Live"
                    })),
            )
            .footer(
                h_flex()
                    .w_full()
                    .justify_between()
                    .child(
                        Button::new("pause-resource")
                            .outline()
                            .small()
                            .label(if monitor.paused { "Resume" } else { "Pause" })
                            .on_click(|_, _, cx| {
                                monitor::global(cx).update(cx, |m, cx| m.set_paused(!m.paused, cx));
                            }),
                    )
                    .child(
                        Button::new("done-resource")
                            .small()
                            .label("Done")
                            .on_click(|_, window, cx| window.close_sheet(cx)),
                    ),
            )
            .child(
                v_flex()
                    .id("resource-details")
                    .size_full()
                    .overflow_y_scroll()
                    .gap_5()
                    .child(body),
            )
    });
}

fn rows(items: Vec<(&'static str, String)>, cx: &App) -> impl IntoElement {
    v_flex()
        .p_4()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .bg(theme::palette(cx).card)
        .children(items.into_iter().map(|(label, value)| {
            h_flex()
                .justify_between()
                .gap_4()
                .py_3()
                .border_b_1()
                .border_color(cx.theme().border.opacity(0.5))
                .text_sm()
                .child(div().text_color(cx.theme().muted_foreground).child(label))
                .child(div().text_right().child(value))
        }))
}

fn note(text: &'static str, cx: &App) -> impl IntoElement {
    div()
        .p_4()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().muted.opacity(0.4))
        .text_sm()
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
        .h(px(112.))
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
    controls: (&Rc<RefCell<Selection>>, &gpui_kit::Entity<InputState>),
    cx: &App,
) -> AnyElement {
    let (selection, search) = controls;
    let s = &frame.stats;
    let identity = &s.details.identity;
    let palette = theme::palette(cx);
    match resource {
        Resource::Cpu => {
            let average = (!s.cpu_usage.is_empty()).then(|| {
                s.cpu_usage.iter().map(|v| *v as f64).sum::<f64>() / s.cpu_usage.len() as f64
            });
            v_flex()
                .gap_2()
                .child(heading(
                    identity
                        .cpu_model
                        .clone()
                        .unwrap_or_else(|| "Processor".into()),
                ))
                .child(rows(
                    vec![
                        (
                            "Average utilization",
                            unavailable_or(average, |v| format!("{v:.1}%")),
                        ),
                        (
                            "Physical cores",
                            unavailable_or(identity.physical_cores, |v| v.to_string()),
                        ),
                        ("Logical processors", s.cpu_usage.len().to_string()),
                    ],
                    cx,
                ))
                .child(heading("Utilization over time"))
                .child(chart(history, |s| s.cpu, palette.charts[0], unavailable))
                .child(heading("Per-core activity"))
                .child(note("Each core is scaled from 0 to 100%.", cx))
                .child(div().grid().grid_cols(2).gap_4().children(
                    s.cpu_usage.iter().enumerate().map(|(i, v)| {
                        v_flex()
                            .gap_1p5()
                            .child(
                                h_flex()
                                    .justify_between()
                                    .text_xs()
                                    .child(
                                        div()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!("Core {i}")),
                                    )
                                    .child(format!("{v:.1}%")),
                            )
                            .child(
                                Progress::new(("core-detail", i))
                                    .value(v.clamp(0.0, 100.0))
                                    .color(palette.charts[0]),
                            )
                    }),
                ))
                .child(note(
                    "The overview averages utilization across logical processors. A process can \
                     exceed 100% CPU when it uses several cores.",
                    cx,
                ))
                .into_any_element()
        }
        Resource::Memory => v_flex()
            .gap_4()
            .child(
                div()
                    .text_3xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format_bytes(s.memory_used)),
            )
            .child(note("RAM in use", cx))
            .child(chart(history, |s| s.memory, palette.charts[2], unavailable))
            .child(rows(
                vec![
                    ("Physical memory", format_bytes(s.memory_total)),
                    ("In use", format_bytes(s.memory_used)),
                    ("Not in use", format_bytes(s.memory_free)),
                    ("Unallocated", format_bytes(s.details.memory_unallocated)),
                ],
                cx,
            ))
            .child(
                v_flex()
                    .p_4()
                    .gap_3()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(heading("Swap"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Memory the operating system stores on disk."),
                    )
                    .when(s.details.swap_total > 0, |this| {
                        this.child(
                            h_flex()
                                .justify_between()
                                .text_sm()
                                .child(format!("{} used", format_bytes(s.details.swap_used)))
                                .child(format!("of {}", format_bytes(s.details.swap_total))),
                        )
                        .child(
                            Progress::new("swap-used")
                                .value(
                                    (s.details.swap_used as f64 / s.details.swap_total as f64
                                        * 100.)
                                        .clamp(0., 100.) as f32,
                                )
                                .color(palette.charts[2]),
                        )
                    })
                    .when(s.details.swap_total == 0, |this| {
                        this.child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("No swap space is currently allocated."),
                        )
                    }),
            )
            .child(note(
                "Not in use is total RAM minus reported use. Unallocated is the OS free-page \
                 count. These can overlap, and swap use alone does not measure memory pressure.",
                cx,
            ))
            .into_any_element(),
        Resource::Gpu => {
            if frame.gpus.is_empty() {
                return note("No graphics adapters reported readings on this system.", cx)
                    .into_any_element();
            }
            v_flex()
                .gap_2()
                .when(frame.gpus.len() > 1, |this| {
                    let selection = selection.clone();
                    let devices = frame.gpus.clone();
                    this.child(heading("Graphics adapter")).child(
                        Button::new("adapter")
                            .outline()
                            .label(
                                frame
                                    .gpus
                                    .iter()
                                    .find(|g| g.id == selection.borrow().gpu)
                                    .unwrap_or(&frame.gpus[0])
                                    .name
                                    .clone(),
                            )
                            .dropdown_menu(move |mut menu, _, _| {
                                for g in &devices {
                                    let selection = selection.clone();
                                    let id = g.id.clone();
                                    menu = menu.item(PopupMenuItem::new(g.name.clone()).on_click(
                                        move |_, _, cx| {
                                            selection.borrow_mut().gpu = id.clone();
                                            cx.refresh_windows();
                                        },
                                    ));
                                }
                                menu
                            }),
                    )
                })
                .children(
                    frame
                        .gpus
                        .iter()
                        .filter(|g| {
                            g.id == selection.borrow().gpu
                                || (selection.borrow().gpu.is_empty() && g.id == frame.gpus[0].id)
                        })
                        .map(|g| gpu_details(g, history, &palette, unavailable, cx)),
                )
                .into_any_element()
        }
        Resource::Storage => {
            let volumes = &s.details.volumes;
            v_flex()
                .gap_2()
                .when(volumes.is_empty(), |this| {
                    this.child(note("No volumes reported.", cx))
                })
                .child(heading("Volume"))
                .child({
                    let selection = selection.clone();
                    let volumes = volumes.clone();
                    Button::new("volume-picker")
                        .outline()
                        .label(
                            volumes
                                .iter()
                                .find(|v| v.mount == selection.borrow().volume)
                                .or_else(|| volumes.iter().find(|v| v.mount == "/"))
                                .or_else(|| volumes.first())
                                .map_or("No volumes".to_string(), |v| {
                                    format!("{} · {}", v.mount, v.name)
                                }),
                        )
                        .dropdown_menu(move |mut menu, _, _| {
                            for v in &volumes {
                                let selection = selection.clone();
                                let mount = v.mount.clone();
                                menu = menu.item(
                                    PopupMenuItem::new(format!("{} · {}", v.mount, v.name))
                                        .on_click(move |_, _, cx| {
                                            selection.borrow_mut().volume = mount.clone();
                                            cx.refresh_windows();
                                        }),
                                );
                            }
                            menu
                        })
                })
                .children(
                    volumes
                        .iter()
                        .enumerate()
                        .filter(|(_, v)| {
                            let mount = selection.borrow().volume.clone();
                            if mount.is_empty() {
                                v.mount
                                    == volumes
                                        .iter()
                                        .find(|v| v.mount == "/")
                                        .or_else(|| volumes.first())
                                        .map_or("", |v| v.mount.as_str())
                            } else {
                                v.mount == mount
                            }
                        })
                        .map(|(i, v)| {
                            let used = v.total.saturating_sub(v.available);
                            let percent = if v.total > 0 {
                                used as f32 / v.total as f32 * 100.0
                            } else {
                                0.0
                            };
                            v_flex()
                                .gap_1()
                                .child(heading(format!(
                                    "{} · {}",
                                    v.mount,
                                    if v.name.is_empty() { "Volume" } else { &v.name }
                                )))
                                .child(div().text_3xl().font_weight(FontWeight::SEMIBOLD).child(
                                    if v.total > 0 {
                                        format_bytes(v.available)
                                    } else {
                                        "Unavailable".into()
                                    },
                                ))
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("Available on this volume"),
                                )
                                .child(
                                    Progress::new(("volume", i))
                                        .value(percent)
                                        .color(palette.charts[3]),
                                )
                                .child(
                                    h_flex()
                                        .justify_between()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(if v.total > 0 {
                                            format!("{percent:.1}% used")
                                        } else {
                                            "Unavailable".into()
                                        })
                                        .child(if v.total > 0 {
                                            format!("{:.1}% available", 100. - percent)
                                        } else {
                                            String::new()
                                        }),
                                )
                                .child(rows(
                                    vec![
                                        (
                                            "Capacity",
                                            if v.total > 0 {
                                                format_bytes(v.total)
                                            } else {
                                                "Unavailable".into()
                                            },
                                        ),
                                        (
                                            "Used",
                                            if v.total > 0 {
                                                format_bytes(used)
                                            } else {
                                                "Unavailable".into()
                                            },
                                        ),
                                        (
                                            "Available",
                                            if v.total > 0 {
                                                format_bytes(v.available)
                                            } else {
                                                "Unavailable".into()
                                            },
                                        ),
                                        (
                                            "File system",
                                            if v.file_system.is_empty() {
                                                "Unavailable".into()
                                            } else {
                                                v.file_system.clone()
                                            },
                                        ),
                                        (
                                            "Access",
                                            if v.read_only {
                                                "Read only".into()
                                            } else {
                                                "Read and write".into()
                                            },
                                        ),
                                    ],
                                    cx,
                                ))
                        }),
                )
                .child(note(
                    "These are file-system capacity readings, not a scan of files. Volumes can \
                     share one storage pool (APFS), so their capacities can overlap.",
                    cx,
                ))
                .into_any_element()
        }
        Resource::System => {
            v_flex()
                .gap_2()
                .child(heading(
                    identity
                        .os_version
                        .clone()
                        .unwrap_or_else(|| "Operating system unavailable".into()),
                ))
                .child(rows(
                    vec![
                        ("Uptime", format_uptime(s.uptime)),
                        (
                            "Kernel",
                            identity
                                .kernel_version
                                .clone()
                                .unwrap_or_else(|| "Unavailable".into()),
                        ),
                        ("Architecture", identity.architecture.clone()),
                        (
                            "Processor",
                            identity
                                .cpu_model
                                .clone()
                                .unwrap_or_else(|| "Unavailable".into()),
                        ),
                        ("Logical processors", s.cpu_usage.len().to_string()),
                    ],
                    cx,
                ))
                .child(heading("Load average"))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Queued and running work over time."),
                )
                .child(h_flex().gap_3().children(s.load_avg.iter().enumerate().map(
                    |(i, value)| {
                        v_flex()
                            .flex_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!(
                                        "{} minute{}",
                                        [1, 5, 15][i],
                                        if i == 0 { "" } else { "s" }
                                    )),
                            )
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(format!("{value:.2}")),
                            )
                    },
                )))
                .child(note(
                    "Load is not a CPU percentage. Uptime is the time since the operating system \
                     started.",
                    cx,
                ))
                .into_any_element()
        }
        Resource::Network => {
            let rate = |v: Option<u64>| unavailable_or(v, |b| format!("{}/s", format_bytes(b)));
            let mut interfaces = s.details.interfaces.clone();
            if selection.borrow().interface_order.is_empty() {
                interfaces.sort_by(|a, b| {
                    (b.received.unwrap_or(0).saturating_add(b.sent.unwrap_or(0)))
                        .cmp(&a.received.unwrap_or(0).saturating_add(a.sent.unwrap_or(0)))
                        .then_with(|| {
                            b.received_total
                                .saturating_add(b.sent_total)
                                .cmp(&a.received_total.saturating_add(a.sent_total))
                        })
                        .then_with(|| a.name.cmp(&b.name))
                });
                selection.borrow_mut().interface_order =
                    interfaces.iter().map(|i| i.name.clone()).collect();
            }
            interfaces.sort_by_key(|i| {
                selection
                    .borrow()
                    .interface_order
                    .iter()
                    .position(|name| name == &i.name)
                    .unwrap_or(usize::MAX)
            });
            let query = search.read(cx).value().trim().to_lowercase();
            let unused = selection.borrow().unused;
            let total = interfaces.len();
            interfaces.retain(|i| {
                (unused
                    || !query.is_empty()
                    || i.received_total > 0
                    || i.sent_total > 0
                    || i.received.is_none()
                    || i.sent.is_none())
                    && i.name.to_lowercase().contains(&query)
            });
            let shown = interfaces.len();
            let toggle = selection.clone();
            v_flex()
                .gap_3()
                .child(rows(
                    vec![
                        ("Received", rate(s.network_rx_bytes)),
                        ("Sent", rate(s.network_tx_bytes)),
                    ],
                    cx,
                ))
                .child(div().h(px(112.)).flex().child(stats::network_trend(
                    history,
                    &palette,
                    unavailable,
                )))
                .child(heading(format!("Interfaces · {shown} of {total}")))
                .child(Input::new(search).cleanable(true))
                .child(
                    Checkbox::new("include-unused")
                        .label("Include unused interfaces")
                        .checked(unused)
                        .on_click(move |checked, _, cx| {
                            toggle.borrow_mut().unused = *checked;
                            cx.refresh_windows();
                        }),
                )
                .when(interfaces.is_empty(), |this| {
                    this.child(note(
                        if !query.is_empty() {
                            "No interfaces match this search."
                        } else if total > 0 {
                            "No interfaces with recorded traffic."
                        } else {
                            "No network interfaces were reported."
                        },
                        cx,
                    ))
                })
                .when(interfaces.is_empty() && !query.is_empty(), |this| {
                    let search = search.clone();
                    this.child(
                        Button::new("clear-interface-search")
                            .link()
                            .small()
                            .label("Clear search")
                            .on_click(move |_, window, cx| {
                                search.update(cx, |input, cx| input.set_value("", window, cx));
                            }),
                    )
                })
                .when(
                    interfaces.is_empty() && query.is_empty() && total > 0 && !unused,
                    |this| {
                        let selection = selection.clone();
                        this.child(
                            Button::new("show-all-interfaces")
                                .link()
                                .small()
                                .label("Show all interfaces")
                                .on_click(move |_, _, cx| {
                                    selection.borrow_mut().unused = true;
                                    cx.refresh_windows();
                                }),
                        )
                    },
                )
                .children(interfaces.into_iter().map(|i| {
                    v_flex()
                        .py_3()
                        .border_b_1()
                        .border_color(cx.theme().border.opacity(0.5))
                        .p_4()
                        .rounded(cx.theme().radius)
                        .border_1()
                        .border_color(cx.theme().border)
                        .child(
                            div()
                                .text_sm()
                                .font_family(cx.theme().mono_font_family.clone())
                                .child(i.name.clone()),
                        )
                        .child(
                            div()
                                .grid()
                                .grid_cols(2)
                                .gap_4()
                                .py_3()
                                .child(definition("↓ Received /s", rate(i.received), cx))
                                .child(definition("↑ Sent /s", rate(i.sent), cx)),
                        )
                        .child(
                            div()
                                .grid()
                                .grid_cols(2)
                                .gap_4()
                                .pt_2()
                                .border_t_1()
                                .border_color(cx.theme().border)
                                .child(definition(
                                    "Received total",
                                    format_bytes(i.received_total),
                                    cx,
                                ))
                                .child(definition("Sent total", format_bytes(i.sent_total), cx)),
                        )
                }))
                .child(note(
                    "Rates use the shared sampling interval. Totals can reset when an interface \
                     restarts, and virtual interfaces can count the same traffic twice.",
                    cx,
                ))
                .into_any_element()
        }
    }
}

fn definition(label: &'static str, value: String, cx: &App) -> impl IntoElement {
    v_flex()
        .gap_1()
        .text_xs()
        .child(div().text_color(cx.theme().muted_foreground).child(label))
        .child(value)
}

fn gpu_details(
    g: &crate::gpu::GpuDevice,
    history: &[Sample],
    palette: &theme::Palette,
    unavailable: bool,
    cx: &App,
) -> impl IntoElement {
    let id = g.id.clone();
    let facts = vec![
        (
            "GPU utilization",
            unavailable_or(g.utilization, |v| format!("{v:.1}%")),
        ),
        (
            if g.memory_kind == "shared" {
                "Shared memory in use"
            } else {
                "GPU memory in use"
            },
            unavailable_or(g.memory_used, format_bytes),
        ),
        (
            "GPU memory capacity",
            if g.memory_kind == "shared" {
                "Uses system memory".into()
            } else {
                unavailable_or(g.memory_total, format_bytes)
            },
        ),
        (
            "Temperature",
            unavailable_or(g.temperature, |v| format!("{v:.0} °C")),
        ),
        (
            "Power",
            unavailable_or(g.power_watts, |v| format!("{v:.1} W")),
        ),
        (
            "Core clock",
            unavailable_or(g.core_clock_mhz, |v| format!("{v:.0} MHz")),
        ),
    ];
    let shared = if g.memory_kind == "shared" {
        "Shared GPU memory is part of your system memory, not additional VRAM. "
    } else {
        ""
    };
    let explanation = format!(
        "{}. {shared}Unavailable counters are not reported as zero. \
         Support depends on the GPU and its driver.",
        g.source,
    );
    v_flex()
        .gap_3()
        .child(heading(g.name.clone()))
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(format!(
                    "{}{}",
                    g.vendor,
                    g.cores
                        .map_or(String::new(), |n| format!(" · {n} graphics cores"))
                )),
        )
        .child(rows(facts, cx))
        .child(chart(
            history,
            move |s| s.gpus.get(&id).copied().flatten(),
            palette.charts[1],
            unavailable,
        ))
        .child(
            v_flex()
                .p_4()
                .gap_2()
                .rounded_lg()
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().muted.opacity(0.4))
                .child(heading("About these readings"))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(explanation),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(if monitor::global(cx).read(cx).paused {
                            "Updates paused in both windows."
                        } else {
                            "History covers the last two minutes and stays in memory."
                        }),
                ),
        )
}
