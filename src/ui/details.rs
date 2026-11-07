//! Process details sheet and the end-process confirmation.
use super::monitor;
use super::process_view::{format_bytes, format_mb, format_timestamp, identity, Column, Identity};
use crate::monitoring::{ProcessInfo, ProcessMonitor};
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Sizable, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    div, px, AnyElement, App, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window,
};

const MAX_CHILDREN: usize = 50;

/// Opens the details sheet. It reads the latest frame on every render, so
/// numbers stay live while it is open.
pub fn open(id: Identity, window: &mut Window, cx: &mut App) {
    window.open_sheet(cx, move |sheet, _, cx| {
        let frame = monitor::global(cx).read(cx).frame.clone();
        let process = frame
            .as_ref()
            .and_then(|f| f.processes.iter().find(|p| identity(p) == id));
        let Some(p) = process else {
            return sheet.title("Process details").size(px(460.)).child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("This process is no longer available."),
            );
        };
        let all = &frame.as_ref().expect("frame present").processes;
        let name = p.name.clone();
        sheet
            .size(px(460.))
            .title(
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
                            .child(format!("PID {} · {}", p.pid, p.user)),
                    ),
            )
            .child(body(p, all, cx))
            .footer(
                h_flex().justify_end().w_full().child(
                    Button::new("end-process")
                        .danger()
                        .label("End Process")
                        .on_click(move |_, window, cx| confirm_kill(id, name.clone(), window, cx)),
                ),
            )
    });
}

fn metric(label: &'static str, value: String, cx: &App) -> impl IntoElement {
    v_flex()
        .flex_1()
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
                .text_sm()
                .font_family(cx.theme().mono_font_family.clone())
                .child(value),
        )
}

fn field(label: &'static str, value: impl IntoElement, cx: &App) -> impl IntoElement {
    h_flex()
        .gap_3()
        .py_1()
        .text_sm()
        .child(
            div()
                .w(px(96.))
                .flex_shrink_0()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(div().min_w_0().flex_1().child(value))
}

fn section(title: &'static str, cx: &App) -> impl IntoElement {
    div()
        .pt_3()
        .pb_1()
        .text_xs()
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(cx.theme().muted_foreground)
        .child(title)
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

fn body(p: &ProcessInfo, all: &[ProcessInfo], cx: &App) -> impl IntoElement {
    let parent = all
        .iter()
        .find(|q| q.pid == p.ppid && q.pid != p.pid && q.start_time <= p.start_time);
    let children: Vec<&ProcessInfo> = all
        .iter()
        .filter(|q| q.ppid == p.pid && q.pid != p.pid)
        .collect();
    let mono = cx.theme().mono_font_family.clone();
    v_flex()
        .id("process-details")
        .size_full()
        .overflow_y_scroll()
        .gap_1()
        .child(
            h_flex()
                .gap_2()
                .child(metric("CPU", format!("{:.1}%", p.cpu_usage), cx))
                .child(metric("Memory", format_mb(p.memory_usage), cx))
                .child(metric("GPU", Column::Gpu.text(p), cx)),
        )
        .child(section("Process", cx))
        .child(field("Status", p.status.clone(), cx))
        .child(field("User", p.user.clone(), cx))
        .child(field(
            "Parent",
            match parent {
                Some(parent) => link(
                    "parent".into(),
                    parent,
                    format!("{} ({})", parent.name, parent.pid),
                ),
                None => p.ppid.to_string().into_any_element(),
            },
            cx,
        ))
        .child(field("Started", format_timestamp(p.start_time), cx))
        .child(field("Run time", Column::RunTime.text(p), cx))
        .when_some(p.threads, |this, threads| {
            this.child(field("Threads", threads.to_string(), cx))
        })
        .when_some(p.session_id, |this, session| {
            this.child(field("Session", session.to_string(), cx))
        })
        .child(section("Resources", cx))
        .child(field("Virtual", format_bytes(p.virtual_memory), cx))
        .child(field(
            "Disk I/O",
            format!(
                "{} read · {} written",
                format_bytes(p.disk_usage.0),
                format_bytes(p.disk_usage.1)
            ),
            cx,
        ))
        .when(!p.ports.is_empty(), |this| {
            this.child(field(
                "Ports",
                p.ports
                    .iter()
                    .map(u16::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
                cx,
            ))
        })
        .child(section("Command", cx))
        .child(
            div()
                .p_2()
                .rounded(cx.theme().radius)
                .bg(cx.theme().secondary)
                .font_family(mono.clone())
                .text_xs()
                .child(if p.command.is_empty() {
                    "—".to_string()
                } else {
                    p.command.clone()
                }),
        )
        .child(field("Root", p.root.clone(), cx))
        .when(!children.is_empty(), |this| {
            this.child(section("Children", cx)).child(
                v_flex()
                    .items_start()
                    .children(
                        children
                            .iter()
                            .take(MAX_CHILDREN)
                            .enumerate()
                            .map(|(i, child)| {
                                link(
                                    format!("child-{i}").into(),
                                    child,
                                    format!("{} ({})", child.name, child.pid),
                                )
                            }),
                    )
                    .when(children.len() > MAX_CHILDREN, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("and {} more", children.len() - MAX_CHILDREN)),
                        )
                    }),
            )
        })
        .when(!p.environ.is_empty(), |this| {
            this.child(section("Environment", cx)).child(
                v_flex()
                    .p_2()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().secondary)
                    .font_family(mono)
                    .text_xs()
                    .children(p.environ.iter().map(|line| div().child(line.clone()))),
            )
        })
}

/// Asks before signalling; the process is re-identified by start time first.
pub fn confirm_kill(id: Identity, name: String, window: &mut Window, cx: &mut App) {
    window.open_alert_dialog(cx, move |dialog, _, _| {
        let name = name.clone();
        dialog
            .title("End process?")
            .description(format!(
                "{name} (PID {}) will be terminated. Unsaved work in it may be lost.",
                id.0
            ))
            .ok_text("End Process")
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
                            "Could not end {name}. It may have exited, or it needs higher privileges."
                        )),
                        cx,
                    );
                }
                true
            })
    });
}
