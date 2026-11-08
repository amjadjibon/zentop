// Copyright (c) 2026 Tobi Lütke
// SPDX-License-Identifier: MIT
// Adapted from tobi/disktree 9b18b272ca3c5a145f6ff58eaf89dc4d311eeaea, crates/disktree-app/src/widgets.rs.
// Full license: licenses/disktree-MIT.md. See UPSTREAM.md for adapters.

//! Reused upstream breadcrumb, selection, meter, legend and key-hint widgets.
use super::{
    scale::{space, text},
    ActiveDiskTheme,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, relative, App, Div, ElementId, FontWeight, Hsla, InteractiveElement as _, ParentElement,
    SharedString, Styled,
};
use zentop_core::disktree::share;
pub fn crumb(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    active: bool,
    cx: &App,
) -> gpui_kit::Stateful<Div> {
    let theme = cx.disk_theme();
    let color = if active {
        theme.bright
    } else {
        theme.secondary
    };
    div()
        .id(id)
        .px(space::XS)
        .py(space::XXS)
        .text_size(text::BODY)
        .text_color(color)
        .when(active, |this| this.font_weight(FontWeight::SEMIBOLD))
        .when(!active, |this| {
            this.hover(|style| style.bg(theme.hover_fill()))
        })
        .child(label.into())
}

pub fn eyebrow(label: impl Into<SharedString>, cx: &App) -> Div {
    let theme = cx.disk_theme();
    let label: SharedString = label.into();
    div()
        .text_size(text::CAPTION)
        .text_color(theme.secondary.opacity(0.7))
        .whitespace_nowrap()
        .child(SharedString::from(label.to_uppercase()))
}

pub fn figure(
    label: impl Into<SharedString>,
    value: impl Into<SharedString>,
    color: Hsla,
    cx: &App,
) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(space::XXS)
        .min_w_0()
        .child(eyebrow(label, cx))
        .child(
            div()
                .text_size(text::TITLE)
                .text_color(color)
                .whitespace_nowrap()
                .overflow_hidden()
                .child(value.into()),
        )
}

pub fn split_size(text: &str) -> (String, String) {
    text.split_once(' ').map_or_else(
        || (text.to_string(), String::new()),
        |(number, unit)| (number.to_string(), unit.to_string()),
    )
}

pub fn measure(
    number: impl Into<SharedString>,
    number_size: gpui_kit::Rems,
    unit: impl Into<SharedString>,
    unit_size: gpui_kit::Rems,
    cx: &App,
) -> Div {
    let theme = cx.disk_theme();
    let lift = gpui_kit::Rems((number_size.0 - unit_size.0) * DESCENT);
    div()
        .flex()
        .flex_row()
        .items_end()
        .gap(space::SM)
        .child(
            div()
                .text_size(number_size)
                .line_height(number_size)
                .font_weight(FontWeight::BOLD)
                .text_color(theme.bright)
                .child(number.into()),
        )
        .child(
            div()
                .text_size(unit_size)
                .line_height(unit_size)
                .pb(lift)
                .text_color(theme.secondary)
                .whitespace_nowrap()
                .child(unit.into()),
        )
}

pub fn bar(fraction: f32, color: Hsla, cx: &App) -> Div {
    let theme = cx.disk_theme();
    div()
        .relative()
        .w_full()
        .h(super::scale::size::METER)
        .bg(theme.foreground.opacity(0.08))
        .child(
            div()
                .absolute()
                .left_0()
                .top_0()
                .h_full()
                .w(relative(fraction.clamp(0.0, 1.0)))
                .bg(color),
        )
}

pub fn row(label: impl Into<SharedString>, value: impl Into<SharedString>, cx: &App) -> Div {
    let theme = cx.disk_theme();
    div()
        .flex()
        .flex_row()
        .justify_between()
        .gap(space::SM)
        .text_size(text::CAPTION)
        .child(div().text_color(theme.secondary).child(label.into()))
        .child(div().min_w_0().text_color(theme.bright).child(value.into()))
}

pub fn chip(label: impl Into<SharedString>, color: Hsla, cx: &App) -> Div {
    let theme = cx.disk_theme();
    div()
        .px(space::XS)
        .py(space::XXS)
        .border_1()
        .border_color(color.opacity(0.5))
        .bg(color.opacity(0.1))
        .text_color(color)
        .text_size(text::CAPTION)
        .font_family(theme.font.clone())
        .child(label.into())
}

pub fn percent(part: u64, total: u64) -> String {
    let value = share(part, total);
    if value < 9.95 {
        format!("{value:.1}%")
    } else {
        format!("{value:.0}%")
    }
}

pub fn meter_row(
    label: impl Into<SharedString>,
    value: impl Into<SharedString>,
    fraction: f32,
    color: Hsla,
    cx: &App,
) -> Div {
    let theme = cx.disk_theme();
    div()
        .flex()
        .flex_col()
        .gap(space::XS)
        .child(
            div()
                .flex()
                .flex_row()
                .justify_between()
                .text_size(text::CAPTION)
                .text_color(theme.secondary)
                .child(div().child(label.into()))
                .child(
                    div()
                        .text_color(theme.bright)
                        .font_weight(FontWeight::MEDIUM)
                        .child(value.into()),
                ),
        )
        .child(
            div()
                .w_full()
                .h(space::XS)
                .bg(theme.foreground.opacity(0.08))
                .child(
                    div()
                        .h_full()
                        .w(relative(fraction.clamp(0.0, 1.0)))
                        .bg(color),
                ),
        )
}

pub fn hint(keys: impl Into<SharedString>, label: impl Into<SharedString>, cx: &App) -> Div {
    let theme = cx.disk_theme();
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(space::XS)
        .child(super::keycap(keys, cx))
        .child(
            div()
                .text_size(text::CAPTION)
                .text_color(theme.secondary)
                .child(label.into()),
        )
}

pub fn swatch(color: Hsla) -> Div {
    div()
        .flex_shrink_0()
        .size(super::scale::size::SWATCH)
        .bg(color)
}

pub fn hatch_swatch(color: Hsla, ground: Hsla) -> Div {
    div()
        .flex_shrink_0()
        .size(super::scale::size::SWATCH)
        .bg(ground)
        .child(
            div()
                .size_full()
                .bg(gpui_kit::pattern_slash(color, 1.0, 3.0)),
        )
}

const DESCENT: f32 = 0.2;
