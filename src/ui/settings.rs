//! The Settings window. Every change is saved through the shared model, so the
//! other windows pick it up immediately.
use super::monitor::{self, Monitor, REFRESH_RATES};
use super::theme;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable, Icon, Sizable, TitleBar};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    div, px, AnyElement, App, AppContext, Context, Entity, FontWeight, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Window,
};
use serde_json::{json, Value};
use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};

fn fold_theme(value: &str) -> String {
    value
        .nfkd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn matches_theme(label: &str, query: &str) -> bool {
    fold_theme(label).contains(&fold_theme(query))
}

pub(super) const THEME_GROUPS: &[(&str, &[&str])] = &[
    (
        "Dark",
        &[
            "catppuccin",
            "dracula",
            "monokaiPro",
            "tokyoNight",
            "solarizedDark",
            "ayuDark",
            "ayuMirage",
        ],
    ),
    (
        "Light",
        &["githubLight", "solarizedLight", "oneLight", "ayuLight"],
    ),
    ("Warm", &["gruvbox"]),
    ("Cool", &["nord", "oneDark"]),
    (
        "Fun",
        &[
            "bubblegum",
            "rosePine",
            "cottonCandy",
            "synthwave",
            "candyfloss",
        ],
    ),
    ("Retro", &["terminal", "amber", "ibmPC"]),
    ("Accessibility", &["highContrast"]),
    ("Glassy", &["glassy"]),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    General,
    Appearance,
}

impl Pane {
    const ALL: [Pane; 2] = [Pane::General, Pane::Appearance];
    fn label(self) -> &'static str {
        match self {
            Pane::General => "General",
            Pane::Appearance => "Appearance",
        }
    }
    fn icon(self) -> IconName {
        match self {
            Pane::General => IconName::Settings,
            Pane::Appearance => IconName::Palette,
        }
    }
}

pub struct Settings {
    monitor: Entity<Monitor>,
    pane: Pane,
    _observe: Subscription,
    search: Entity<InputState>,
    _search_subscription: Subscription,
}

impl Settings {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let monitor = monitor::global(cx);
        let observe = cx.observe(&monitor, |_, _, cx| cx.notify());
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Find a theme…"));
        let subscription = cx.subscribe_in(&search, window, |_, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        Self {
            search,
            _search_subscription: subscription,
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
    let menu_bar = m.behavior_bool("menuBarEnabled");
    let keep_running = m.behavior_bool("keepRunning");
    let rate_m = monitor.clone();
    let bar_m = monitor.clone();
    let keep_m = monitor.clone();
    let card = |title| {
        v_flex()
            .p_4()
            .gap_4()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(theme::palette(cx).card)
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
    };
    v_flex()
        .gap_5()
        .child(
            card("Monitoring")
                .child(row(
                    "Refresh interval",
                    "Applies to the app and menu-bar overview.",
                    Button::new("rate")
                        .outline()
                        .label(format!(
                            "{}s",
                            super::super::sampling::interval(rate) as f64 / 1000.
                        ))
                        .dropdown_caret(true)
                        .dropdown_menu(move |mut menu, _, _| {
                            for ms in REFRESH_RATES {
                                let monitor = rate_m.clone();
                                menu = menu.item(
                                    PopupMenuItem::new(format!("{}s", ms / 1000))
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
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            "Longer intervals reduce background activity. Pause and resume from \
                             the app bar.",
                        ),
                ),
        )
        .child(
            card("Menu-bar companion")
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            Checkbox::new("menu-bar")
                                .label("Show menu-bar icon")
                                .checked(menu_bar)
                                .on_click(move |checked, _, cx| {
                                    save(
                                        &bar_m,
                                        json!({"behavior":{"menuBarEnabled":*checked}}),
                                        cx,
                                    )
                                }),
                        )
                        .child(
                            div()
                                .pl_6()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(
                                    "Keep CPU, memory, graphics and network readings close at \
                                     hand.",
                                ),
                        ),
                )
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            Checkbox::new("keep-running")
                                .label("Keep running when the window closes")
                                .checked(keep_running)
                                .disabled(!menu_bar)
                                .on_click(move |checked, _, cx| {
                                    save(&keep_m, json!({"behavior":{"keepRunning":*checked}}), cx)
                                }),
                        )
                        .child(
                            div()
                                .pl_6()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(
                                    "Continue from the menu bar. Choose Quit Zentop to exit \
                                     completely.",
                                ),
                        ),
                ),
        )
        .into_any_element()
}

fn appearance(monitor: &Entity<Monitor>, query: &str, cx: &App) -> AnyElement {
    let current = monitor.read(cx).theme_name().to_owned();
    let mut gallery = v_flex().gap_4();
    for (group, names) in THEME_GROUPS {
        let matches: Vec<_> = names
            .iter()
            .filter_map(|name| theme::all().find(|(key, _)| key == name))
            .filter(|(_, label)| matches_theme(label, query))
            .collect();
        if matches.is_empty() {
            continue;
        }
        let cards = div()
            .grid()
            .grid_cols(2)
            .gap_1()
            .children(matches.into_iter().map(|(name, label)| {
                let preview = theme::preview(name).expect("known theme");
                let selected = name == current;
                let monitor = monitor.clone();
                h_flex()
                    .id(SharedString::from(name))
                    .min_h(px(36.))
                    .px_3()
                    .py_2()
                    .gap_2()
                    .rounded(cx.theme().radius)
                    .text_xs()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(cx.theme().secondary))
                    .hover(|d| d.bg(cx.theme().accent))
                    .child(
                        h_flex().gap_0p5().children(
                            preview
                                .accents
                                .into_iter()
                                .take(3)
                                .map(|color| div().size_2().rounded_full().bg(color)),
                        ),
                    )
                    .child(label)
                    .child(div().flex_1())
                    .when(selected, |d| d.child(Icon::new(IconName::Check).xsmall()))
                    .on_click(move |_, _, cx| monitor.update(cx, |m, cx| m.set_theme(name, cx)))
            }));
        gallery = gallery.child(
            v_flex()
                .gap_1()
                .child(
                    div()
                        .px_2()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(*group),
                )
                .child(cards),
        );
    }
    gallery.into_any_element()
}

impl Render for Settings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let error = self.monitor.read(cx).prefs_error.clone();
        let content = match self.pane {
            Pane::General => general(&self.monitor, cx),
            Pane::Appearance => {
                let query = self.search.read(cx).value().to_string();
                let matches = theme::all().any(|(_, label)| matches_theme(label, &query));
                v_flex()
                    .gap_3()
                    .child(
                        Input::new(&self.search)
                            .cleanable(true)
                            .prefix(Icon::new(IconName::Search).small()),
                    )
                    .child(appearance(&self.monitor, &query, cx))
                    .when(!matches, |this| {
                        this.child(
                            div()
                                .p_3()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("No matching themes."),
                        )
                    })
                    .into_any_element()
            }
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
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    .child(
                        h_flex()
                            .w_full()
                            .p_4()
                            .justify_center()
                            .gap_1()
                            .bg(theme::palette(cx).card)
                            .border_b_1()
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
                            .w_full()
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
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(match self.pane {
                                        Pane::General => "Choose how Zentop monitors your system.",
                                        Pane::Appearance => {
                                            "All your Zentop themes, across every window."
                                        }
                                    }),
                            )
                            .child(content),
                    ),
            )
            .child(
                div()
                    .px_6()
                    .py_3()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .bg(theme::palette(cx).card)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Changes are saved automatically."),
            )
    }
}

/// The title-bar picker uses the same searchable gallery as Settings.
pub(super) struct ThemePicker {
    monitor: Entity<Monitor>,
    search: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}
impl ThemePicker {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let monitor = monitor::global(cx);
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Find a theme…"));
        let subscriptions = vec![
            cx.observe(&monitor, |_, _, cx| cx.notify()),
            cx.subscribe(&search, |_, _, _: &InputEvent, cx| cx.notify()),
        ];
        Self {
            monitor,
            search,
            _subscriptions: subscriptions,
        }
    }
}
impl Render for ThemePicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.search.read(cx).value().to_string();
        v_flex()
            .w(px(352.))
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child("Choose a theme"),
            )
            .child(
                Input::new(&self.search)
                    .cleanable(true)
                    .prefix(Icon::new(IconName::Search).small()),
            )
            .child(
                v_flex()
                    .id("theme-picker-list")
                    .max_h(px(440.))
                    .overflow_y_scroll()
                    .child(appearance(&self.monitor, &query, cx)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::matches_theme;
    #[test]
    fn theme_search_folds_accents_case_and_whitespace() {
        for query in ["rose pine", "  ROSE   pine ", "Rosé Pine", "rose\tpine", ""] {
            assert!(matches_theme("Rosé Pine", query), "{query:?}");
        }
        assert!(matches_theme("Rose\u{301} Pine", "rosé pine"));
        assert!(matches_theme("Tokyo Night", "NIGHT"));
        assert!(!matches_theme("Rosé Pine", "rose dark"));
    }
}
