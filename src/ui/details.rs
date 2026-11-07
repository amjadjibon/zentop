//! Process details sheet and the end-process confirmation.
use super::monitor;
use super::process_view::{format_bytes, format_timestamp, identity, Column, Identity};
use crate::monitoring::{ProcessInfo, ProcessMonitor};
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Sizable, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    div, px, AnyElement, App, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window,
};
use std::{cell::Cell, rc::Rc};

const MAX_CHILDREN: usize = 50;

/// Opens the details sheet. It reads the latest frame on every render, so
/// numbers stay live while it is open.
pub fn open(id: Identity, window: &mut Window, cx: &mut App) {
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
            )
            .child(body(p, all, &environment_open, cx))
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

fn body(
    p: &ProcessInfo,
    all: &[ProcessInfo],
    environment_open: &Rc<Cell<bool>>,
    cx: &App,
) -> impl IntoElement {
    let parent = all
        .iter()
        .find(|q| q.pid == p.ppid && q.pid != p.pid && q.start_time <= p.start_time);
    let children: Vec<_> = all
        .iter()
        .filter(|q| q.ppid == p.pid && q.pid != p.pid)
        .collect();
    let mono = cx.theme().mono_font_family.clone();
    let toggle = environment_open.clone();
    v_flex().id("process-details").size_full().overflow_y_scroll().gap_6()
        .child(div().grid().grid_cols(2).gap_4()
            .child(card("CPU usage", cx)
                .child(div().text_2xl().font_weight(gpui_kit::FontWeight::SEMIBOLD).child(format!("{:.1}%", p.cpu_usage)))
                .child(Progress::new("process-cpu").value(p.cpu_usage.clamp(0.,100.)).color(if p.cpu_usage > 80. { super::theme::palette(cx).danger } else if p.cpu_usage > 50. { super::theme::palette(cx).warning } else { super::theme::palette(cx).charts[0] })))
            .child(card("Resident memory", cx)
                .child(div().text_2xl().font_weight(gpui_kit::FontWeight::SEMIBOLD).child(format_bytes(p.memory_usage)))
                .child(div().text_xs().text_color(cx.theme().muted_foreground).child(format!("Virtual: {}",format_bytes(p.virtual_memory))))))
        .child(card("Process information", cx).child(div().grid().grid_cols(2).gap_4()
            .child(field("Name", p.name.clone(), cx)).child(field("User",p.user.clone(),cx))
            .child(field("Session ID",p.session_id.map_or_else(|| "Unavailable".into(), |v| v.to_string()),cx))
            .child(field("Threads",p.threads.map_or_else(|| "Unavailable".into(), |v| v.to_string()),cx))
            .child(field("Started",format_timestamp(p.start_time),cx))
            .child(field("Run time",Column::RunTime.text(p),cx))
            .child(field("Parent process",match parent { Some(parent) => link("parent".into(),parent,format!("{} · {} ↗",parent.name,parent.pid)), None => div().text_xs().child(format!("{} · not in this snapshot",p.ppid)).into_any_element() },cx))
            .child(field("Disk I/O",div().text_xs().child(format!("Read {} · Written {}",format_bytes(p.disk_usage.0),format_bytes(p.disk_usage.1))),cx))))
        .child(card("Command & root",cx)
            .child(field("Command",div().id("command-block").max_h(px(176.)).overflow_y_scroll().p_3().border_1().border_color(cx.theme().border).rounded_md().bg(cx.theme().background).font_family(mono.clone()).text_xs().child(if p.command.is_empty(){"Unavailable".into()}else{p.command.clone()}),cx))
            .child(field("Root",div().text_xs().font_family(mono.clone()).child(if p.root.is_empty(){"Unavailable".into()}else{p.root.clone()}),cx)))
        .child(card("Child processes",cx)
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(format!("{} children",children.len())))
            .when(children.is_empty(),|d| d.child(div().text_sm().text_color(cx.theme().muted_foreground).child("No child process in the current snapshot.")))
            .when(!children.is_empty(),|d| d.child(h_flex().text_xs().gap_3().child(div().flex_1().child("Name")).child("PID").child("CPU").child("Memory")))
            .children(children.iter().take(MAX_CHILDREN).enumerate().map(|(i,child)| h_flex().gap_3().py_2().border_t_1().border_color(cx.theme().border).text_xs()
                .child(div().flex_1().min_w_0().child(link(format!("child-{i}").into(),child,format!("{} ↗",child.name))))
                .child(child.pid.to_string()).child(format!("{:.1}%",child.cpu_usage)).child(format_bytes(child.memory_usage)))))
        .child(Button::new("environment-toggle").outline().label(format!("Environment variables ({}) {}",p.environ.len(),if environment_open.get(){"⌃"}else{"⌄"})).on_click(move |_,_,cx| { toggle.set(!toggle.get());cx.refresh_windows(); }))
        .when(environment_open.get(), |d| d.child(v_flex().gap_3()
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child("Values reported by the operating system for this process."))
            .child(v_flex().id("environment-block").max_h(px(288.)).overflow_y_scroll().p_4().gap_2().rounded_md().border_1().border_color(cx.theme().border).font_family(mono).text_xs()
                .children(p.environ.iter().map(|v| div().child(v.clone())))
                .when(p.environ.is_empty(),|d| d.child("No accessible environment variables reported.")))))
        .child(div().text_xs().text_color(cx.theme().muted_foreground).child("Readings follow the process snapshot. Virtual address space is not physical RAM; CPU can exceed 100% across cores."))
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
                            "Could not end {name}. It may have exited, or it needs higher privileges."
                        )),
                        cx,
                    );
                }
                true
            })
    });
}
