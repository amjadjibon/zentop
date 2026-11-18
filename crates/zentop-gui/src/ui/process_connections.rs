//! On-demand socket snapshots for the process inspector.
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    div, px, App, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled,
};
use std::{cell::RefCell, rc::Rc};
use zentop_core::{
    connections::{Protocol, Report},
    process_view::Identity,
};

pub struct State {
    identity: Identity,
    result: Option<Result<Report, String>>,
    protocol: Option<Protocol>,
}
pub type Model = Rc<RefCell<State>>;

pub fn new(identity: Identity, cx: &mut App) -> Model {
    let model = Rc::new(RefCell::new(State {
        identity,
        result: None,
        protocol: None,
    }));
    refresh(&model, cx);
    model
}
fn refresh(model: &Model, cx: &mut App) {
    let identity = model.borrow().identity;
    model.borrow_mut().result = None;
    let weak = Rc::downgrade(model);
    cx.spawn(async move |cx| {
        let result = cx
            .background_executor()
            .spawn(async move { zentop_core::connections::collect(identity.0, identity.1) })
            .await;
        if let Some(model) = weak.upgrade() {
            model.borrow_mut().result = Some(result);
            cx.update(|cx| cx.refresh_windows());
        }
    })
    .detach();
}

pub fn render(model: &Model, cx: &App) -> impl IntoElement {
    let state = model.borrow();
    let refresh_model = model.clone();
    let mut card = v_flex()
        .gap_3()
        .p_4()
        .min_w_0()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .bg(super::theme::palette(cx).card)
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Network connections"),
                )
                .child(
                    Button::new("refresh-connections")
                        .outline()
                        .small()
                        .label("Refresh")
                        .disabled(state.result.is_none())
                        .on_click(move |_, _, cx| {
                            refresh(&refresh_model, cx);
                            cx.refresh_windows();
                        }),
                ),
        )
        .child(
            h_flex().gap_2().children(
                [
                    (None, "All"),
                    (Some(Protocol::Tcp), "TCP"),
                    (Some(Protocol::Udp), "UDP"),
                ]
                .into_iter()
                .enumerate()
                .map(|(i, (protocol, label))| {
                    let model = model.clone();
                    Button::new(("connection-protocol", i))
                        .ghost()
                        .small()
                        .label(label)
                        .when(state.protocol == protocol, |b| b.outline())
                        .on_click(move |_, _, cx| {
                            model.borrow_mut().protocol = protocol;
                            cx.refresh_windows();
                        })
                }),
            ),
        );
    let note = |text: String| {
        div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(text)
    };
    match &state.result {
        None => card = card.child(note("Checking connections…".into())),
        Some(Err(error)) => card = card.child(note(error.clone())),
        Some(Ok(report)) => {
            card = card.child(note(format!(
                "{} sockets · Snapshot {}",
                report.connections.len(),
                super::process_view::format_timestamp(report.captured_at)
            )));
            if report.partial {
                card = card.child(note(
                    "Some sockets could not be read; results are partial.".into(),
                ));
            }
            if report.truncated {
                card = card.child(note(
                    "Showing the first 512 sockets; more were reported.".into(),
                ));
            }
            let rows: Vec<_> = report
                .connections
                .iter()
                .filter(|c| state.protocol.is_none_or(|p| c.protocol == p))
                .collect();
            if rows.is_empty() {
                card = card.child(note(
                    "No accessible connections reported for this selection.".into(),
                ));
            } else {
                card = card.child(
                    v_flex()
                        .id("connection-rows")
                        .max_h(px(320.))
                        .overflow_y_scroll()
                        .gap_2()
                        .children(rows.into_iter().enumerate().map(|(i, c)| {
                            let local = c.local.to_string();
                            let remote = c.remote_label();
                            v_flex()
                                .gap_1()
                                .py_2()
                                .border_t_1()
                                .border_color(cx.theme().border)
                                .child(
                                    h_flex()
                                        .justify_between()
                                        .text_xs()
                                        .child(format!("{} · {}", c.protocol.label(), c.state))
                                        .when(c.state == "LISTEN", |row| {
                                            row.child(if c.local.ip().is_unspecified() {
                                                "All interfaces"
                                            } else if c.local.ip().is_loopback() {
                                                "This computer only"
                                            } else {
                                                "Specific interface"
                                            })
                                        }),
                                )
                                .child(
                                    h_flex().gap_4().children(
                                        [
                                            ("Local", local, true),
                                            ("Remote", remote, c.remote.is_some()),
                                        ]
                                        .into_iter()
                                        .map(
                                            |(label, address, copy)| {
                                                v_flex()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .child(
                                                        h_flex()
                                                            .gap_1()
                                                            .text_xs()
                                                            .text_color(cx.theme().muted_foreground)
                                                            .child(label)
                                                            .when(copy, |row| {
                                                                row.child(super::copy_button(
                                                                    SharedString::from(format!(
                                                                        "connection-{i}-{label}"
                                                                    )),
                                                                    "address",
                                                                    address.clone(),
                                                                ))
                                                            }),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .font_family(
                                                                cx.theme().mono_font_family.clone(),
                                                            )
                                                            .child(address),
                                                    )
                                            },
                                        ),
                                    ),
                                )
                        })),
                );
            }
        }
    }
    card.child(note("Snapshot on open; Refresh checks again. Permissions may hide sockets. UDP peer addresses are unavailable.".into()))
}
