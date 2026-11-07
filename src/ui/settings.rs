//! The Settings window. Every change is saved through the shared model, so the
//! other windows pick it up immediately.
use super::monitor::{self, Monitor, REFRESH_RATES};
use super::process_view::Column;
use super::theme;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable, Icon, Sizable, TitleBar};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    div, px, AnyElement, App, Context, Entity, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Window,
};
use serde_json::{json, Value};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    General,
    Appearance,
    Columns,
}

impl Pane {
    const ALL: [Pane; 3] = [Pane::General, Pane::Appearance, Pane::Columns];
    fn label(self) -> &'static str {
        match self {
            Pane::General => "General",
            Pane::Appearance => "Appearance",
            Pane::Columns => "Columns",
        }
    }
    fn icon(self) -> IconName {
        match self {
            Pane::General => IconName::Settings,
            Pane::Appearance => IconName::Palette,
            Pane::Columns => IconName::Columns3,
        }
    }
}

const STATUS_FILTERS: [(&str, &str); 5] = [
    ("all", "All statuses"),
    ("running", "Running"),
    ("sleeping", "Sleeping"),
    ("idle", "Idle"),
    ("unknown", "Unknown"),
];

pub struct Settings {
    monitor: Entity<Monitor>,
    pane: Pane,
    _observe: Subscription,
}

impl Settings {
    pub fn new(_: &mut Window, cx: &mut Context<Self>) -> Self {
        let monitor = monitor::global(cx);
        let observe = cx.observe(&monitor, |_, _, cx| cx.notify());
        Self {
            monitor,
            pane: Pane::General,
            _observe: observe,
        }
    }
}

fn save(monitor: &Entity<Monitor>, patch: Value, cx: &mut App) {
    monitor.update(cx, |m, cx| m.save(patch, cx));
}

fn row(
    title: &'static str,
    detail: &'static str,
    control: impl IntoElement,
    cx: &App,
) -> impl IntoElement {
    h_flex()
        .gap_4()
        .py_3()
        .border_b_1()
        .border_color(cx.theme().border.opacity(0.6))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().text_sm().child(title))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(detail),
                ),
        )
        .child(control)
}

fn general(monitor: &Entity<Monitor>, cx: &App) -> AnyElement {
    let m = monitor.read(cx);
    let rate = m.refresh_rate();
    let tree = m.prefs().behavior("processView").as_str() == Some("tree");
    let status = m
        .prefs()
        .behavior("defaultStatusFilter")
        .as_str()
        .unwrap_or("all")
        .to_owned();
    let menu_bar = m.behavior_bool("menuBarEnabled");
    let keep_running = m.behavior_bool("keepRunning");
    let status_label = STATUS_FILTERS
        .iter()
        .find(|(value, _)| *value == status)
        .map_or("All statuses", |(_, label)| label);
    let (rate_m, list_m, tree_m, status_m, bar_m, keep_m) = (
        monitor.clone(),
        monitor.clone(),
        monitor.clone(),
        monitor.clone(),
        monitor.clone(),
        monitor.clone(),
    );
    v_flex()
        .child(row(
            "Refresh rate",
            "How often readings are captured. Shorter intervals use more CPU.",
            Button::new("rate")
                .small()
                .outline()
                .label(format!("Every {}s", rate / 1000))
                .dropdown_caret(true)
                .dropdown_menu(move |mut menu, _, _| {
                    for ms in REFRESH_RATES {
                        let monitor = rate_m.clone();
                        menu = menu.item(
                            PopupMenuItem::new(format!("Every {}s", ms / 1000))
                                .checked(rate == ms)
                                .on_click(move |_, _, cx| {
                                    monitor.update(cx, |m, cx| m.set_refresh_rate(ms, cx))
                                }),
                        );
                    }
                    menu
                }),
            cx,
        ))
        .child(row(
            "Process view",
            "Show processes as a flat list or grouped into families.",
            h_flex()
                .child(
                    Button::new("view-list")
                        .small()
                        .with_variant(if tree {
                            ButtonVariant::Ghost
                        } else {
                            ButtonVariant::Secondary
                        })
                        .icon(IconName::List)
                        .label("List")
                        .on_click(move |_, _, cx| {
                            save(&list_m, json!({"behavior": {"processView": "flat"}}), cx)
                        }),
                )
                .child(
                    Button::new("view-tree")
                        .small()
                        .with_variant(if tree {
                            ButtonVariant::Secondary
                        } else {
                            ButtonVariant::Ghost
                        })
                        .icon(IconName::ListTree)
                        .label("Tree")
                        .on_click(move |_, _, cx| {
                            save(&tree_m, json!({"behavior": {"processView": "tree"}}), cx)
                        }),
                ),
            cx,
        ))
        .child(row(
            "Default status filter",
            "Applied when the main window opens.",
            Button::new("status")
                .small()
                .outline()
                .label(status_label)
                .dropdown_caret(true)
                .dropdown_menu(move |mut menu, _, _| {
                    for (value, label) in STATUS_FILTERS {
                        let monitor = status_m.clone();
                        menu =
                            menu.item(PopupMenuItem::new(label).checked(status == value).on_click(
                                move |_, _, cx| {
                                    save(
                                        &monitor,
                                        json!({"behavior": {"defaultStatusFilter": value}}),
                                        cx,
                                    )
                                },
                            ));
                    }
                    menu
                }),
            cx,
        ))
        .child(row(
            "Menu-bar overview",
            "Show the NeoHtop icon in the menu bar or system tray.",
            Switch::new("menu-bar")
                .checked(menu_bar)
                .on_click(move |checked, _, cx| {
                    save(
                        &bar_m,
                        json!({"behavior": {"menuBarEnabled": *checked}}),
                        cx,
                    )
                }),
            cx,
        ))
        .child(row(
            "Keep running",
            "Continue monitoring from the menu bar after closing the main window.",
            Switch::new("keep-running")
                .checked(keep_running && menu_bar)
                .disabled(!menu_bar)
                .on_click(move |checked, _, cx| {
                    save(&keep_m, json!({"behavior": {"keepRunning": *checked}}), cx)
                }),
            cx,
        ))
        .into_any_element()
}

fn appearance(monitor: &Entity<Monitor>, cx: &App) -> AnyElement {
    let current = monitor.read(cx).theme_name().to_owned();
    div()
        .grid()
        .grid_cols(3)
        .gap_3()
        .children(theme::all().enumerate().filter_map(|(i, (name, label))| {
            let preview = theme::preview(name)?;
            let selected = name == current;
            let monitor = monitor.clone();
            Some(
                v_flex()
                    .id(("theme", i))
                    .cursor_pointer()
                    .rounded(cx.theme().radius)
                    .h(px(104.))
                    .border_2()
                    .border_color(if selected {
                        cx.theme().primary
                    } else {
                        cx.theme().border
                    })
                    .overflow_hidden()
                    .on_click(move |_, _, cx| monitor.update(cx, |m, cx| m.set_theme(name, cx)))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_h_0()
                            .p_2()
                            .gap_1()
                            .bg(preview.background)
                            .child(
                                div()
                                    .h(px(6.))
                                    .w(px(64.))
                                    .rounded_sm()
                                    .bg(preview.foreground),
                            )
                            .child(div().h(px(6.)).w(px(40.)).rounded_sm().bg(preview.muted))
                            .child(h_flex().gap_1().mt_1().children(
                                preview.accents.map(|c| div().size_3().rounded_full().bg(c)),
                            )),
                    )
                    .child(
                        h_flex()
                            .justify_between()
                            .px_2()
                            .py_1p5()
                            .bg(preview.card)
                            .border_t_1()
                            .border_color(preview.border)
                            .text_xs()
                            .text_color(preview.foreground)
                            .child(label)
                            .when(selected, |this| {
                                this.child(
                                    Icon::new(IconName::Check)
                                        .xsmall()
                                        .text_color(preview.foreground),
                                )
                            }),
                    ),
            )
        }))
        .into_any_element()
}

fn columns(monitor: &Entity<Monitor>, cx: &App) -> AnyElement {
    let prefs = monitor.read(cx).prefs();
    v_flex()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("Process Name is always shown."),
        )
        .children(
            Column::ALL
                .into_iter()
                .filter(|c| *c != Column::Name)
                .enumerate()
                .map(|(i, column)| {
                    let visible = prefs.column_visible(column.key()) == Some(true);
                    let monitor = monitor.clone();
                    Checkbox::new(("column", i))
                        .label(column.label())
                        .checked(visible)
                        .on_click(move |checked, _, cx| {
                            save(
                                &monitor,
                                json!({"appearance": {"columnVisibility": {column.key(): *checked}}}),
                                cx,
                            )
                        })
                }),
        )
        .into_any_element()
}

impl Render for Settings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let error = self.monitor.read(cx).prefs_error.clone();
        let content = match self.pane {
            Pane::General => general(&self.monitor, cx),
            Pane::Appearance => appearance(&self.monitor, cx),
            Pane::Columns => columns(&self.monitor, cx),
        };
        let danger = theme::palette(cx).danger;
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                TitleBar::new().child(
                    div()
                        .w_full()
                        .when(cfg!(target_os = "macos"), |this| this.pl(px(72.)))
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Settings"),
                ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    .child(
                        v_flex()
                            .w(px(180.))
                            .h_full()
                            .p_2()
                            .gap_0p5()
                            .bg(theme::palette(cx).card)
                            .border_r_1()
                            .border_color(cx.theme().border)
                            .children(Pane::ALL.map(|pane| {
                                let selected = pane == self.pane;
                                h_flex()
                                    .id(SharedString::from(pane.label()))
                                    .gap_2()
                                    .px_2()
                                    .py_1p5()
                                    .rounded(cx.theme().radius)
                                    .text_sm()
                                    .cursor_pointer()
                                    .when(selected, |this| this.bg(cx.theme().secondary))
                                    .when(!selected, |this| {
                                        this.hover(|this| this.bg(cx.theme().accent))
                                    })
                                    .child(
                                        Icon::new(pane.icon())
                                            .small()
                                            .text_color(cx.theme().muted_foreground),
                                    )
                                    .child(pane.label())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.pane = pane;
                                        cx.notify();
                                    }))
                            })),
                    )
                    .child(
                        v_flex()
                            .id("settings-pane")
                            .flex_1()
                            .h_full()
                            .overflow_y_scroll()
                            .p_6()
                            .gap_4()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(self.pane.label()),
                            )
                            .when_some(error, |this, error| {
                                this.child(
                                    div()
                                        .px_3()
                                        .py_2()
                                        .rounded(cx.theme().radius)
                                        .border_1()
                                        .border_color(danger)
                                        .text_sm()
                                        .text_color(danger)
                                        .child(error),
                                )
                            })
                            .child(content),
                    ),
            )
    }
}
