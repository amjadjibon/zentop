//! The Settings window. Every change is saved through the shared model, so the
//! other windows pick it up immediately.
use super::monitor::{self, Monitor, REFRESH_RATES};
use super::theme;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants};
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
    Alerts,
}

impl Pane {
    const ALL: [Pane; 3] = [Pane::General, Pane::Appearance, Pane::Alerts];
    fn label(self) -> &'static str {
        match self {
            Pane::General => "General",
            Pane::Appearance => "Appearance",
            Pane::Alerts => "Alerts",
        }
    }
    fn icon(self) -> IconName {
        match self {
            Pane::General => IconName::Settings,
            Pane::Appearance => IconName::Palette,
            Pane::Alerts => IconName::SlidersHorizontal,
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

fn alerts(monitor: &Entity<Monitor>, cx: &App) -> AnyElement {
    use zentop_core::alerts::Kind;
    let m = monitor.read(cx);
    let settings = m.features.config.alerts.clone();
    let mut content = v_flex().gap_3();
    for (i, kind) in Kind::ALL.into_iter().enumerate() {
        let rule = settings.rule(kind);
        let toggle = monitor.clone();
        let threshold = monitor.clone();
        let options: &[u8] = if kind == Kind::DiskFree {
            &[1, 2, 5, 10, 15, 20, 25, 30]
        } else {
            &[50, 60, 70, 75, 80, 85, 90, 95, 99]
        };
        let selected = rule.threshold;
        content = content.child(
            h_flex()
                .py_3()
                .gap_3()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(
                    Checkbox::new(("alert-enabled", i))
                        .checked(rule.enabled)
                        .label(kind.label())
                        .on_click(move |enabled, _, cx| {
                            toggle.update(cx, |m, cx| {
                                let mut settings = m.features.config.alerts.clone();
                                settings.rule_mut(kind).enabled = *enabled;
                                m.set_alerts(settings, cx);
                            });
                        }),
                )
                .child(div().flex_1())
                .child(
                    Button::new(("alert-threshold", i))
                        .outline()
                        .label(format!(
                            "{} {selected}%",
                            if kind == Kind::DiskFree {
                                "At or below"
                            } else {
                                "At or above"
                            }
                        ))
                        .dropdown_caret(true)
                        .dropdown_menu(move |mut menu, _, _| {
                            for value in options {
                                let monitor = threshold.clone();
                                let value = *value;
                                menu = menu.item(
                                    PopupMenuItem::new(format!("{value}%"))
                                        .checked(value == selected)
                                        .on_click(move |_, _, cx| {
                                            monitor.update(cx, |m, cx| {
                                                let mut settings = m.features.config.alerts.clone();
                                                settings.rule_mut(kind).threshold = value;
                                                m.set_alerts(settings, cx);
                                            });
                                        }),
                                );
                            }
                            menu
                        }),
                ),
        );
    }
    for (i, (title, selected, options)) in [
        (
            "Sustained for",
            settings.sustain_secs,
            vec![5, 10, 15, 30, 60, 120, 300],
        ),
        (
            "Repeat cooldown",
            settings.cooldown_secs,
            vec![30, 60, 120, 300, 600, 1800, 3600],
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let timing = monitor.clone();
        content = content.child(row(
            title,
            if i == 0 {
                "Ignore shorter spikes."
            } else {
                "Minimum time between notifications for the same resource."
            },
            Button::new(("alert-timing", i))
                .outline()
                .label(format!("{selected}s"))
                .dropdown_caret(true)
                .dropdown_menu(move |mut menu, _, _| {
                    for value in &options {
                        let monitor = timing.clone();
                        let value = *value;
                        menu = menu.item(
                            PopupMenuItem::new(format!("{value}s"))
                                .checked(value == selected)
                                .on_click(move |_, _, cx| {
                                    monitor.update(cx, |m, cx| {
                                        let mut settings = m.features.config.alerts.clone();
                                        if i == 0 {
                                            settings.sustain_secs = value;
                                        } else {
                                            settings.cooldown_secs = value;
                                        }
                                        m.set_alerts(settings, cx);
                                    })
                                }),
                        );
                    }
                    menu
                }),
            cx,
        ));
    }
    let notifications = monitor.clone();
    content = content.child(Checkbox::new("alert-notifications").checked(settings.desktop_notifications).label("Also send desktop notifications").on_click(move |enabled,_,cx| notifications.update(cx, |m,cx| { let mut settings = m.features.config.alerts.clone(); settings.desktop_notifications = *enabled; m.set_alerts(settings,cx); })))
        .child(div().text_xs().text_color(cx.theme().muted_foreground).child("Opt-in rules keep sampling while windows are hidden. Pause suspends alerts. Disk alerts use the least-free writable volume. Desktop delivery depends on OS permissions."));
    if let Some(error) = &m.notification_error {
        content = content.child(
            div()
                .text_sm()
                .text_color(theme::palette(cx).danger)
                .child(error.clone()),
        );
    }
    let clear = monitor.clone();
    content = content.child(
        h_flex()
            .mt_3()
            .justify_between()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child("Recent alerts"),
            )
            .child(
                Button::new("clear-alerts")
                    .small()
                    .ghost()
                    .label("Clear history")
                    .disabled(m.alerts.history.is_empty())
                    .on_click(move |_, _, cx| {
                        clear.update(cx, |m, cx| {
                            m.alerts.history.clear();
                            cx.notify();
                        })
                    }),
            ),
    );
    if m.alerts.history.is_empty() {
        content = content.child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("No alerts this session."),
        );
    }
    for event in &m.alerts.history {
        content = content.child(
            v_flex()
                .py_2()
                .gap_1()
                .child(div().text_sm().child(event.message.clone()))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(super::process_view::format_timestamp(
                            event.timestamp / 1000,
                        )),
                ),
        );
    }
    content.into_any_element()
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

fn appearance(
    monitor: &Entity<Monitor>,
    query: &str,
    picker: Option<Entity<ThemePicker>>,
    cx: &App,
) -> AnyElement {
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
                let picker = picker.clone();
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
                    .on_click(move |_, window, cx| {
                        monitor.update(cx, |m, cx| m.set_theme(name, cx));
                        if let Some(picker) = &picker {
                            picker.update(cx, |this, cx| {
                                this.open = false;
                                this.search
                                    .update(cx, |input, cx| input.set_value("", window, cx));
                                cx.notify();
                            });
                        }
                    })
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
        let error = self
            .monitor
            .read(cx)
            .prefs_error
            .clone()
            .or_else(|| self.monitor.read(cx).feature_error.clone());
        let content = match self.pane {
            Pane::General => general(&self.monitor, cx),
            Pane::Alerts => alerts(&self.monitor, cx),
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
                    .child(appearance(&self.monitor, &query, None, cx))
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
                                Button::new(SharedString::from(pane.label()))
                                    .with_variant(if selected {
                                        ButtonVariant::Secondary
                                    } else {
                                        ButtonVariant::Ghost
                                    })
                                    .icon(pane.icon())
                                    .label(pane.label())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.pane = pane;
                                        if pane == Pane::Alerts {
                                            this.monitor.update(cx, |m, cx| m.reload_features(cx));
                                        }
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
                                        Pane::Alerts => {
                                            "Get notified about sustained resource pressure."
                                        }
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
    pub open: bool,
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
            open: false,
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
                    .child(appearance(&self.monitor, &query, Some(cx.entity()), cx)),
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
