// Copyright (c) 2026 Tobi Lütke
// SPDX-License-Identifier: MIT
// Adapted from tobi/disktree 9b18b272ca3c5a145f6ff58eaf89dc4d311eeaea,
// crates/disktree-app/src/views.rs: selection_section, worth_section, mark_row, legend, side_panel.
// Full license: licenses/disktree-MIT.md. See UPSTREAM.md for adapters.

use super::super::{clean, DiskTree};
use super::{
    palette,
    scale::{size, space, text},
    widgets, Theme,
};
use gpui_kit::component::{
    button::{Button, ButtonVariants},
    h_flex, v_flex, Sizable,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    div, px, App, Context, Div, FontWeight, InteractiveElement, ParentElement,
    StatefulInteractiveElement, Styled,
};
use zentop_core::disktree::{self as disk, Category, Metric, Review};

fn selection_section(app: &DiskTree, theme: &Theme, cx: &Context<DiskTree>) -> Div {
    let section = v_flex()
        .gap(space::MD)
        .child(widgets::eyebrow("Selection", cx));
    let Some(view) = &app.session.view else {
        return section.child(
            div()
                .text_color(theme.secondary)
                .child("Choose a folder to begin"),
        );
    };
    let node = &view.tree.nodes[view.selected];
    let identity = v_flex()
        .gap(space::XS)
        .min_w_0()
        .child(
            h_flex()
                .gap(space::SM)
                .min_w_0()
                .child(
                    div()
                        .flex_shrink_0()
                        .w(space::XS)
                        .h(text::HEADING)
                        .bg(palette::category_accent(theme, node.category)),
                )
                .child(
                    div()
                        .text_size(text::HEADING)
                        .font_weight(FontWeight::BOLD)
                        .text_color(theme.bright)
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(clean(&node.name.to_string_lossy())),
                ),
        )
        .child(
            div()
                .text_size(text::CAPTION)
                .text_color(theme.secondary)
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .child(clean(&view.tree.path(view.selected).display().to_string())),
        );
    let (number, unit) = if view.options.metric == Metric::Files {
        (
            disk::human_count(view.files[view.selected]),
            "files".to_string(),
        )
    } else {
        widgets::split_size(&disk::format_size(view.bytes[view.selected]))
    };
    let measure = v_flex()
        .gap(space::SM)
        .child(widgets::measure(
            number,
            text::DISPLAY,
            unit,
            text::TITLE,
            cx,
        ))
        .child(widgets::bar(
            (disk::share(view.bytes[view.selected], view.bytes[view.zoom]) / 100.) as f32,
            palette::highlight(theme),
            cx,
        ));
    let grid = v_flex()
        .gap(space::MD)
        .child(
            h_flex()
                .child(div().flex_1().min_w_0().child(widgets::figure(
                    "Of view",
                    widgets::percent(view.bytes[view.selected], view.bytes[view.zoom]),
                    theme.bright,
                    cx,
                )))
                .child(div().flex_1().min_w_0().child(widgets::figure(
                    "Files",
                    disk::human_count(view.files[view.selected]),
                    theme.bright,
                    cx,
                ))),
        )
        .child(
            h_flex()
                .child(div().flex_1().min_w_0().child(widgets::figure(
                    "Last write",
                    disk::age(node.modified),
                    theme.bright,
                    cx,
                )))
                .child(div().flex_1().min_w_0().child(widgets::figure(
                    "Kind",
                    node.category.label(),
                    theme.bright,
                    cx,
                ))),
        );
    section
        .child(identity)
        .child(measure)
        .child(grid)
        .when(node.symlink, |d| {
            d.child(widgets::chip("Symbolic link", theme.secondary, cx))
        })
        .when_some(node.issue.as_ref(), |d, issue| {
            d.child(widgets::chip(clean(issue), theme.warning, cx))
        })
        .child(
            h_flex()
                .gap(space::SM)
                .child(
                    Button::new("disk-mark")
                        .outline()
                        .small()
                        .label(if view.marked.contains(&view.selected) {
                            "Unmark"
                        } else {
                            "Mark"
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.change(|v| v.mark(), cx))),
                )
                .child(
                    Button::new("disk-enter")
                        .ghost()
                        .small()
                        .label("Open")
                        .on_click(cx.listener(|this, _, _, cx| this.change(|v| v.enter(), cx))),
                ),
        )
}

fn worth_section(app: &DiskTree, theme: &Theme, cx: &Context<DiskTree>) -> Div {
    let candidates = app
        .session
        .view
        .as_ref()
        .map_or_else(Vec::new, |v| v.review());
    let total = app
        .session
        .view
        .as_ref()
        .map_or(0, |v| candidates.iter().map(|i| v.bytes[*i]).sum());
    let largest = app
        .session
        .view
        .as_ref()
        .and_then(|v| candidates.first().map(|i| v.bytes[*i]))
        .unwrap_or(1);
    let mut section = v_flex().gap(space::XS).child(
        h_flex()
            .justify_between()
            .pb(space::XS)
            .child(widgets::eyebrow("Worth a look", cx))
            .when(total > 0, |d| {
                d.child(
                    div()
                        .text_size(text::CAPTION)
                        .text_color(palette::highlight(theme))
                        .child(disk::format_size(total)),
                )
            }),
    );
    if candidates.is_empty() {
        return section.child(
            div()
                .text_size(text::CAPTION)
                .text_color(theme.secondary)
                .child(if app.session.view.is_some() {
                    "No review candidates found"
                } else {
                    "Waiting for the scan"
                }),
        );
    }
    let Some(view) = &app.session.view else {
        return section;
    };
    for id in candidates {
        let node = &view.tree.nodes[id];
        let accent = palette::category_accent(theme, node.category);
        let path = view.tree.path(id);
        let title = path.parent().and_then(|p| p.file_name()).map_or_else(
            || clean(&node.name.to_string_lossy()),
            |parent| {
                format!(
                    "{}/{}",
                    clean(&parent.to_string_lossy()),
                    clean(&node.name.to_string_lossy())
                )
            },
        );
        section = section.child(
            div()
                .id(("disk-candidate", id))
                .flex()
                .items_center()
                .gap(space::SM)
                .px(space::SM)
                .py(space::XS)
                .when(view.selected == id, |d| d.bg(theme.hover_fill()))
                .hover(|style| style.bg(theme.hover_fill()))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.change(
                        |v| {
                            v.selected = id;
                            v.zoom = v.tree.nodes[id].parent.unwrap_or(0);
                        },
                        cx,
                    );
                    this.focus.focus(window, cx);
                }))
                .child(div().flex_shrink_0().w(px(2.)).h(space::XL).bg(accent))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_size(text::BODY)
                                .text_color(theme.bright)
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(title),
                        )
                        .child(
                            div()
                                .text_size(text::CAPTION)
                                .text_color(theme.secondary.opacity(0.8))
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(node.review.map_or("Review before removing", Review::hint)),
                        ),
                )
                .child(
                    v_flex()
                        .items_end()
                        .gap(space::XS)
                        .flex_shrink_0()
                        .w(size::ROW_BAR)
                        .child(
                            div()
                                .text_size(text::BODY)
                                .text_color(theme.bright)
                                .child(disk::format_size(view.bytes[id])),
                        )
                        .child(widgets::bar(
                            view.bytes[id] as f32 / largest.max(1) as f32,
                            accent,
                            cx,
                        )),
                ),
        );
    }
    section
}

pub fn sidebar(app: &DiskTree, cx: &Context<DiskTree>) -> Div {
    let theme = Theme::from(cx);
    let mut panel = v_flex()
        .w(px(300.))
        .h_full()
        .min_h_0()
        .flex_shrink_0()
        .p(space::LG)
        .gap(space::LG)
        .bg(theme.surface)
        .border_l_1()
        .border_color(theme.divider())
        .child(selection_section(app, &theme, cx))
        .child(
            div()
                .id("disk-insights")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .border_t_1()
                .border_color(theme.divider())
                .pt(space::LG)
                .child(worth_section(app, &theme, cx)),
        );
    let count = app.session.view.as_ref().map_or(0, |v| v.marked.len());
    let marked_bytes = app.session.view.as_ref().map_or(0, |v| v.marked_bytes());
    panel = panel.child(
        Button::new("disk-review-button")
            .outline()
            .small()
            .label(format!(
                "Review {count} marked · {}",
                disk::format_size(marked_bytes)
            ))
            .on_click(cx.listener(|this, _, _, cx| {
                this.reviewing = true;
                cx.notify();
            })),
    );
    if let Some(volume) = app
        .session
        .view
        .as_ref()
        .and_then(|v| v.tree.volume.as_ref())
    {
        let used = volume.total.saturating_sub(volume.available);
        let (number, unit) = widgets::split_size(&disk::format_size(volume.available));
        panel = panel.child(
            v_flex()
                .gap(space::SM)
                .border_t_1()
                .border_color(theme.divider())
                .pt(space::MD)
                .child(widgets::eyebrow(
                    format!("Disk {}", clean(&volume.mount.display().to_string())),
                    cx,
                ))
                .child(widgets::measure(
                    number,
                    text::FIGURE,
                    format!("{unit} free"),
                    text::BODY,
                    cx,
                ))
                .child(widgets::meter_row(
                    "Used",
                    disk::format_size(used),
                    used as f32 / volume.total.max(1) as f32,
                    theme.accent,
                    cx,
                ))
                .child(widgets::row("Total", disk::format_size(volume.total), cx)),
        );
    }
    panel
}

pub fn legend(app: &DiskTree, cx: &App) -> Div {
    let theme = Theme::from(cx);
    let mut legend = h_flex().gap(space::SM);
    if app
        .session
        .view
        .as_ref()
        .is_some_and(|v| v.options.metric == Metric::Age)
    {
        for (i, (_, label)) in palette::AGE_BUCKETS.iter().enumerate() {
            legend = legend.child(
                h_flex()
                    .gap(space::XS)
                    .child(widgets::swatch(palette::age_accent(&theme, i)))
                    .child(*label),
            );
        }
    } else {
        legend = legend.child(
            h_flex()
                .gap(space::XS)
                .child(widgets::hatch_swatch(palette::hatch(&theme), theme.inset))
                .child("Review"),
        );
        for category in Category::ALL {
            legend = legend.child(
                h_flex()
                    .gap(space::XS)
                    .child(widgets::swatch(palette::category_accent(&theme, category)))
                    .child(category.label()),
            );
        }
    }
    legend.text_size(text::CAPTION).text_color(theme.secondary)
}

pub fn key_hints(cx: &App) -> Div {
    h_flex()
        .flex_wrap()
        .gap(space::MD)
        .px(space::LG)
        .py(space::SM)
        .children(
            [
                ("space", "mark"),
                ("enter", "open"),
                ("⌫", "up"),
                ("c", "review"),
                ("hjkl", "move"),
                ("/", "filter"),
                ("[ ]", "depth"),
                ("t", "mode"),
                ("0", "reset"),
                ("r", "rescan"),
            ]
            .into_iter()
            .map(|(keys, label)| widgets::hint(keys, label, cx)),
        )
}

/// Upstream review row, adapted to flat node IDs and read-only actions.
pub fn mark_row(app: &DiskTree, id: usize, cx: &Context<DiskTree>) -> Div {
    let theme = Theme::from(cx);
    let row = h_flex()
        .gap(space::MD)
        .px(space::MD)
        .py(space::SM)
        .border_b_1()
        .border_color(theme.divider());
    let Some(view) = &app.session.view else {
        return row;
    };
    let node = &view.tree.nodes[id];
    row.child(
        v_flex()
            .gap(space::XXS)
            .min_w_0()
            .flex_1()
            .child(
                div()
                    .text_color(theme.foreground)
                    .child(clean(&node.name.to_string_lossy())),
            )
            .child(
                div()
                    .text_size(text::CAPTION)
                    .text_color(theme.secondary)
                    .child(clean(&view.tree.path(id).display().to_string())),
            ),
    )
    .child(
        div()
            .w(size::SHARE_LANE)
            .flex_shrink_0()
            .child(widgets::bar(
                (disk::share(view.bytes[id], view.bytes[view.zoom]) / 100.) as f32,
                theme.secondary,
                cx,
            )),
    )
    .child(
        div()
            .w(size::SIZE_LANE)
            .flex_shrink_0()
            .flex()
            .justify_end()
            .text_color(theme.bright)
            .child(disk::format_size(view.bytes[id])),
    )
    .child(
        Button::new(("disk-unmark", id))
            .outline()
            .small()
            .label("Unmark")
            .on_click(cx.listener(move |this, _, _, cx| {
                this.change(
                    |v| {
                        v.marked.remove(&id);
                    },
                    cx,
                )
            })),
    )
    .child(
        Button::new(("disk-review-open", id))
            .ghost()
            .small()
            .label("Open")
            .on_click(cx.listener(move |this, _, _, cx| {
                this.reviewing = false;
                this.change(
                    |v| {
                        v.selected = id;
                        v.enter();
                    },
                    cx,
                );
            })),
    )
}
