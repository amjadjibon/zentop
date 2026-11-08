//! Process details sheet and the end-process confirmation.
use super::monitor;
use super::process_view::{
    family_parents, format_bytes, format_timestamp, identity, Column, Identity,
};
use crate::monitoring::{LocalPort, ProcessInfo, ProcessMonitor};
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Sizable, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    div, img, px, AnyElement, App, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window,
};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashMap},
    rc::Rc,
};

const MAX_CHILDREN: usize = 50;

/// Ports looked up when the sheet opened: `None` until the lookup finishes.
type Ports = Rc<RefCell<Option<Result<Vec<LocalPort>, String>>>>;

/// Opens the details sheet. It reads the latest frame on every render, so
/// numbers stay live while it is open.
pub fn open(id: Identity, window: &mut Window, cx: &mut App) {
    let icon = Rc::new(RefCell::new(None));
    let resolved_icon = icon.clone();
    let state = monitor::global(cx).read(cx).state();
    cx.spawn(async move |cx| {
        let resolved = cx
            .background_executor()
            .spawn(async move {
                crate::native_icons::resolve_batch(
                    &state,
                    vec![crate::native_icons::IconRequest {
                        pid: id.0,
                        start_time: id.1,
                    }],
                )
            })
            .await;
        *resolved_icon.borrow_mut() = resolved
            .into_iter()
            .find(|(key, _)| *key == id)
            .and_then(|(_, icon)| icon);
        cx.update(|cx| cx.refresh_windows());
    })
    .detach();
    // Reading sockets scans every process, so it happens once per sheet
    // rather than on every sample.
    let ports: Ports = Rc::new(RefCell::new(None));
    let found_ports = ports.clone();
    cx.spawn(async move |cx| {
        let result = cx
            .background_executor()
            .spawn(async move { crate::monitoring::ports_of(id.0) })
            .await;
        *found_ports.borrow_mut() = Some(result);
        cx.update(|cx| cx.refresh_windows());
    })
    .detach();
    let environment_open = Rc::new(Cell::new(false));
    window.open_sheet(cx, move |sheet, _, cx| {
        let frame = monitor::global(cx).read(cx).frame.clone();
        let process = frame
            .as_ref()
            .and_then(|f| f.processes.iter().find(|p| identity(p) == id));
        let Some(p) = process else {
            return sheet.title("Process details").size(px(768.)).child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("This process is no longer available."),
            );
        };
        let all = &frame.as_ref().expect("frame present").processes;
        sheet
            .size(px(768.))
            .title(
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .size_12()
                            .rounded_lg()
                            .bg(cx.theme().muted)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(match icon.borrow().clone() {
                                Some(icon) => img(icon).size_8().into_any_element(),
                                None => gpui_kit::component::Icon::new(
                                    gpui_kit::assets::IconName::SquareTerminal,
                                )
                                .size_6()
                                .into_any_element(),
                            }),
                    )
                    .child(
                        v_flex()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .child(p.name.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("PID {} · {}", p.pid, p.status)),
                            ),
                    ),
            )
            .child(body(p, all, &ports, &environment_open, cx))
    });
}

fn card(title: &'static str, cx: &App) -> gpui_kit::Div {
    v_flex()
        .p_4()
        .gap_4()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .bg(super::theme::palette(cx).card)
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child(title),
        )
}

fn field(label: &'static str, value: impl IntoElement, cx: &App) -> impl IntoElement {
    v_flex()
        .min_w_0()
        .gap_1()
        .text_sm()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(value)
}

fn link(key: SharedString, target: &ProcessInfo, label: String) -> AnyElement {
    let id = identity(target);
    Button::new(key)
        .link()
        .small()
        .label(label)
        .on_click(move |_, window, cx| open(id, window, cx))
        .into_any_element()
}

/// CPU, resident memory and process count of a process and its family.
fn family_total(
    all: &[ProcessInfo],
    parents: &HashMap<usize, usize>,
    root: usize,
) -> (f64, u64, usize) {
    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    for (&child, &parent) in parents {
        children.entry(parent).or_default().push(child);
    }
    let (mut cpu, mut memory, mut count) = (0., 0, 0);
    let mut stack = vec![root];
    while let Some(i) = stack.pop() {
        let p = &all[i];
        if p.cpu_usage.is_finite() {
            cpu += p.cpu_usage.max(0.) as f64;
        }
        memory += p.memory_usage;
        count += 1;
        stack.extend(children.get(&i).into_iter().flatten().copied());
    }
    (cpu, memory, count)
}

/// Groups sockets into one row per protocol and port, describing where each
/// accepts connections from: `("TCP 3000", "All interfaces")`.
fn port_rows(ports: &[LocalPort]) -> Vec<(String, String)> {
    let mut rows: BTreeMap<(bool, u16), Vec<std::net::IpAddr>> = BTreeMap::new();
    for port in ports {
        rows.entry((port.udp, port.address.port()))
            .or_default()
            .push(port.address.ip());
    }
    rows.into_iter()
        .map(|((udp, port), ips)| {
            let reach = if ips.iter().any(|ip| ip.is_unspecified()) {
                "All interfaces".to_string()
            } else if ips.iter().all(|ip| ip.is_loopback()) {
                "This computer only".to_string()
            } else {
                ips.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            (format!("{} {port}", if udp { "UDP" } else { "TCP" }), reach)
        })
        .collect()
}

fn ports_card(ports: &Ports, cx: &App) -> impl IntoElement {
    let note = |text: String| {
        div()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(text)
    };
    let card = card("Ports", cx);
    match &*ports.borrow() {
        None => card.child(note("Checking…".into())),
        Some(Err(error)) => card.child(note(error.clone())),
        Some(Ok(found)) if found.is_empty() => card.child(note(
            "Not listening on any TCP port or bound to any UDP port. \
             Other users' processes may be hidden without elevated permissions."
                .into(),
        )),
        Some(Ok(found)) => card.children(port_rows(found).into_iter().map(|(port, reach)| {
            h_flex()
                .gap_3()
                .py_2()
                .border_t_1()
                .border_color(cx.theme().border)
                .text_xs()
                .child(
                    div()
                        .w(px(96.))
                        .font_family(cx.theme().mono_font_family.clone())
                        .child(port),
                )
                .child(div().text_color(cx.theme().muted_foreground).child(reach))
        })),
    }
}

fn body(
    p: &ProcessInfo,
    all: &[ProcessInfo],
    ports: &Ports,
    environment_open: &Rc<Cell<bool>>,
    cx: &App,
) -> impl IntoElement {
    let parent = all
        .iter()
        .find(|q| q.pid == p.ppid && q.pid != p.pid && q.start_time <= p.start_time);
    // Children and the family total follow the tree view's rules, so an app's
    // helper processes count as part of it.
    let order: Vec<usize> = (0..all.len()).collect();
    let parents = family_parents(all, &order);
    let me = all.iter().position(|q| identity(q) == identity(p));
    let children: Vec<_> = order
        .iter()
        .filter(|i| me.is_some() && parents.get(i) == me.as_ref())
        .map(|&i| &all[i])
        .collect();
    let family = me.map_or((0., 0, 0), |me| family_total(all, &parents, me));
    let owner = me
        .and_then(|me| parents.get(&me))
        .map(|&i| &all[i])
        .filter(|q| p.owner == Some(q.pid));
    let mono = cx.theme().mono_font_family.clone();
    let toggle = environment_open.clone();
    v_flex()
        .id("process-details")
        .size_full()
        .overflow_y_scroll()
        .gap_6()
        .child(
            div()
                .grid()
                .grid_cols(2)
                .gap_4()
                .child(
                    card("CPU usage", cx)
                        .child(
                            div()
                                .text_2xl()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .child(format!("{:.1}%", p.cpu_usage)),
                        )
                        .child(
                            Progress::new("process-cpu")
                                .value(p.cpu_usage.clamp(0., 100.))
                                .color(if p.cpu_usage > 80. {
                                    super::theme::palette(cx).danger
                                } else if p.cpu_usage > 50. {
                                    super::theme::palette(cx).warning
                                } else {
                                    super::theme::palette(cx).charts[0]
                                }),
                        ),
                )
                .child(
                    card("Resident memory", cx)
                        .child(
                            div()
                                .text_2xl()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .child(format_bytes(p.memory_usage)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("Virtual: {}", format_bytes(p.virtual_memory))),
                        ),
                ),
        )
        .when(family.2 > 1, |d| {
            d.child(
                card("Process family", cx)
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(format!("{:.1}% · {}", family.0, format_bytes(family.1))),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "CPU and memory of this process and the {} child and helper \
                                 processes that work for it. Memory is summed, so pages they \
                                 share count more than once.",
                                family.2 - 1
                            )),
                    ),
            )
        })
        .child(
            card("Process information", cx).child(
                div()
                    .grid()
                    .grid_cols(2)
                    .gap_4()
                    .child(field("Name", p.name.clone(), cx))
                    .child(field("User", p.user.clone(), cx))
                    .child(field(
                        "Session ID",
                        p.session_id
                            .map_or_else(|| "Unavailable".into(), |v| v.to_string()),
                        cx,
                    ))
                    .child(field(
                        "Threads",
                        p.threads
                            .map_or_else(|| "Unavailable".into(), |v| v.to_string()),
                        cx,
                    ))
                    .child(field("Started", format_timestamp(p.start_time), cx))
                    .child(field("Run time", Column::RunTime.text(p), cx))
                    .child(field(
                        "Parent process",
                        match parent {
                            Some(parent) => link(
                                "parent".into(),
                                parent,
                                format!("{} · {} ↗", parent.name, parent.pid),
                            ),
                            None => div()
                                .text_xs()
                                .child(format!("{} · not in this snapshot", p.ppid))
                                .into_any_element(),
                        },
                        cx,
                    ))
                    .when_some(owner, |d, owner| {
                        d.child(field(
                            "Belongs to",
                            link(
                                "owner".into(),
                                owner,
                                format!("{} · {} ↗", owner.name, owner.pid),
                            ),
                            cx,
                        ))
                    })
                    .child(field(
                        "Disk I/O",
                        div().text_xs().child(format!(
                            "Read {} · Written {}",
                            format_bytes(p.disk_usage.0),
                            format_bytes(p.disk_usage.1)
                        )),
                        cx,
                    )),
            ),
        )
        .child(ports_card(ports, cx))
        .child(
            card("Command & root", cx)
                .child(field(
                    "Command",
                    div()
                        .id("command-block")
                        .max_h(px(176.))
                        .overflow_y_scroll()
                        .p_3()
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded_md()
                        .bg(cx.theme().background)
                        .font_family(mono.clone())
                        .text_xs()
                        .child(if p.command.is_empty() {
                            "Unavailable".into()
                        } else {
                            p.command.clone()
                        }),
                    cx,
                ))
                .child(field(
                    "Root",
                    div()
                        .text_xs()
                        .font_family(mono.clone())
                        .child(if p.root.is_empty() {
                            "Unavailable".into()
                        } else {
                            p.root.clone()
                        }),
                    cx,
                )),
        )
        .child(
            card("Child processes", cx)
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("{} children", children.len())),
                )
                .when(children.is_empty(), |d| {
                    d.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("No child process in the current snapshot."),
                    )
                })
                .when(!children.is_empty(), |d| {
                    d.child(
                        h_flex()
                            .text_xs()
                            .gap_3()
                            .child(div().flex_1().child("Name"))
                            .child("PID")
                            .child("CPU")
                            .child("Memory"),
                    )
                })
                .children(
                    children
                        .iter()
                        .take(MAX_CHILDREN)
                        .enumerate()
                        .map(|(i, child)| {
                            h_flex()
                                .gap_3()
                                .py_2()
                                .border_t_1()
                                .border_color(cx.theme().border)
                                .text_xs()
                                .child(div().flex_1().min_w_0().child(link(
                                    format!("child-{i}").into(),
                                    child,
                                    if child.owner == Some(p.pid) {
                                        format!("{} (helper) ↗", child.name)
                                    } else {
                                        format!("{} ↗", child.name)
                                    },
                                )))
                                .child(child.pid.to_string())
                                .child(format!("{:.1}%", child.cpu_usage))
                                .child(format_bytes(child.memory_usage))
                        }),
                ),
        )
        .child(
            Button::new("environment-toggle")
                .outline()
                .label(format!(
                    "Environment variables ({}) {}",
                    p.environ.len(),
                    if environment_open.get() { "⌃" } else { "⌄" }
                ))
                .on_click(move |_, _, cx| {
                    toggle.set(!toggle.get());
                    cx.refresh_windows();
                }),
        )
        .when(environment_open.get(), |d| {
            d.child(
                v_flex()
                    .gap_3()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Values reported by the operating system for this process."),
                    )
                    .child(
                        v_flex()
                            .id("environment-block")
                            .max_h(px(288.))
                            .overflow_y_scroll()
                            .p_4()
                            .gap_2()
                            .rounded_md()
                            .border_1()
                            .border_color(cx.theme().border)
                            .font_family(mono)
                            .text_xs()
                            .children(p.environ.iter().map(|v| div().child(v.clone())))
                            .when(p.environ.is_empty(), |d| {
                                d.child("No accessible environment variables reported.")
                            }),
                    ),
            )
        })
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(
                    "Readings follow the process snapshot. Virtual address space is not physical \
                     RAM; CPU can exceed 100% across cores.",
                ),
        )
}

/// Asks before signalling; the process is re-identified by start time first.
pub fn confirm_kill(id: Identity, name: String, window: &mut Window, cx: &mut App) {
    window.open_alert_dialog(cx, move |dialog, _, _| {
        let name = name.clone();
        dialog
            .title(format!("End {name}?"))
            .description("This stops the selected process. Unsaved work may be lost.")
            .child(div().p_3().child(format!("{name}\nPID {}", id.0)))
            .ok_text("End process")
            .ok_variant(ButtonVariant::Danger)
            .confirm()
            .on_ok(move |_, window, cx| {
                let monitor = monitor::global(cx);
                let state = monitor.read(cx).state();
                let killed = state
                    .sys
                    .lock()
                    .map(|sys| ProcessMonitor::kill_process(&sys, id.0, id.1))
                    .unwrap_or(false);
                if killed {
                    window.close_sheet(cx);
                    monitor.update(cx, |monitor, _| monitor.refresh_now());
                } else {
                    window.push_notification(
                        Notification::error(format!(
                            "Could not end {name}. It may have exited, or it needs higher \
                             privileges."
                        )),
                        cx,
                    );
                }
                true
            })
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_rows_merge_addresses_and_describe_reach() {
        let port = |udp, address: &str| LocalPort {
            udp,
            address: address.parse().unwrap(),
        };
        let rows = port_rows(&[
            port(false, "127.0.0.1:3000"),
            port(false, "[::1]:3000"),
            port(true, "0.0.0.0:5353"),
            port(false, "0.0.0.0:443"),
            port(false, "192.168.1.5:8080"),
        ]);
        let rows: Vec<_> = rows.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        assert_eq!(
            rows,
            [
                ("TCP 443", "All interfaces"),
                ("TCP 3000", "This computer only"),
                ("TCP 8080", "192.168.1.5"),
                ("UDP 5353", "All interfaces"),
            ]
        );
    }
}
