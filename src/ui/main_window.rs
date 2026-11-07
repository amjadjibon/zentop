//! The main window: resource cards and the process table.
use super::monitor::{self, Monitor, REFRESH_RATES};
use super::process_view::{self, Column, Filters, RuleKind, STATUSES};
use super::table::ProcessTable;
use super::{details, stats, theme, OpenAbout, OpenSettings, ShowOverview, SMALL_ICON};
use crate::preferences::Preferences;
use crate::sampling;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState, NumberInput};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::table::{DataTable, TableEvent, TableState};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Icon, Sizable, TitleBar};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    div, img, px, AppContext, Context, Entity, FontWeight, IntoElement, ParentElement, Render,
    SharedString, Styled, Subscription, Window,
};
use serde_json::json;

pub struct Zentop {
    monitor: Entity<Monitor>,
    search_text: String,
    filters: Filters,
    last_sequence: Option<u64>,
    search: Entity<InputState>,
    thresholds: [Entity<InputState>; 4],
    table: Entity<TableState<ProcessTable>>,
    _subscriptions: Vec<Subscription>,
}

impl Zentop {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let monitor = monitor::global(cx);
        let (columns, tree, filters) = {
            let m = monitor.read(cx);
            let prefs = m.prefs();
            let filters = Filters {
                statuses: default_statuses(prefs),
                ..Filters::default()
            };
            (
                visible_columns(prefs),
                prefs.behavior("processView").as_str() == Some("tree"),
                filters,
            )
        };
        let root = cx.weak_entity();
        let state = monitor.read(cx).state();
        let initial_filters = filters.clone();
        let table = cx.new(|cx| {
            let mut delegate = ProcessTable::new(state, root, columns, tree);
            delegate.set_filters(initial_filters);
            TableState::new(delegate, window, cx)
                .col_movable(false)
                .row_selectable(true)
        });
        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search name, command, PID or :port — separate terms with commas")
                .clean_on_escape()
        });
        let defaults = Filters::default();
        let thresholds = RuleKind::ALL.map(|kind| {
            let value = defaults.rule(kind).value.to_string();
            cx.new(|cx| InputState::new(window, cx).default_value(value))
        });
        let mut subscriptions = vec![
            cx.observe(&monitor, |this, _, cx| this.sync(cx)),
            cx.subscribe_in(&search, window, |this, input, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().to_string();
                    this.set_search(text, cx);
                }
            }),
            cx.subscribe_in(
                &table,
                window,
                |_, table, event: &TableEvent, window, cx| {
                    if let TableEvent::DoubleClickedRow(row) = event {
                        if let Some(id) = table.read(cx).delegate().identity_at(*row) {
                            details::open(id, window, cx);
                        }
                    }
                },
            ),
        ];
        for (kind, input) in RuleKind::ALL.into_iter().zip(thresholds.iter()) {
            subscriptions.push(cx.subscribe_in(
                input,
                window,
                move |this, input, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        if let Ok(value) = input.read(cx).value().trim().parse::<f64>() {
                            if value.is_finite() {
                                this.update_filters(cx, |f| f.rule_mut(kind).value = value);
                            }
                        }
                    }
                },
            ));
        }
        let mut this = Self {
            monitor,
            search_text: String::new(),
            filters,
            last_sequence: None,
            search,
            thresholds,
            table,
            _subscriptions: subscriptions,
        };
        this.update_demand(cx);
        this.sync(cx);
        this
    }

    /// Pulls the latest frame and preferences from the shared model.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let monitor = self.monitor.read(cx);
        let frame = monitor.frame.clone();
        let columns = visible_columns(monitor.prefs());
        let tree = monitor.prefs().behavior("processView").as_str() == Some("tree");
        let port_search = self.port_search();
        // A paused hub keeps returning the same frame; skip needless rebuilds.
        let new_frame = frame.filter(|f| port_search || self.last_sequence != Some(f.sequence));
        if let Some(frame) = &new_frame {
            self.last_sequence = Some(frame.sequence);
        }
        self.table.update(cx, |table, cx| {
            let delegate = table.delegate_mut();
            if delegate.columns() != columns.as_slice() {
                delegate.set_columns(columns);
                table.refresh(cx);
            }
            if table.delegate().tree != tree {
                table.delegate_mut().set_tree(tree);
            }
            if let Some(frame) = new_frame {
                table.delegate_mut().set_frame(frame);
                // The visible range may not change when rows arrive.
                let visible = table.visible_range().rows().clone();
                let visible = visible.start..visible.end.max(visible.start + 40);
                table.delegate_mut().load_icons(visible, cx);
            }
            cx.notify();
        });
        self.update_demand(cx);
        cx.notify();
    }

    fn update_demand(&mut self, cx: &mut Context<Self>) {
        let gpu = self.filters.gpu.enabled
            || self
                .monitor
                .read(cx)
                .prefs()
                .column_visible(Column::Gpu.key())
                == Some(true);
        let ports = self.port_search();
        self.monitor
            .update(cx, |monitor, _| monitor.set_demand(gpu, ports));
    }

    fn port_search(&self) -> bool {
        process_view::has_port_query(&self.search_text)
    }

    fn set_search(&mut self, text: String, cx: &mut Context<Self>) {
        self.search_text = text.clone();
        self.update_demand(cx);
        self.table.update(cx, |table, cx| {
            table.delegate_mut().set_search(text);
            cx.notify();
        });
        cx.notify();
    }

    fn update_filters(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Filters)) {
        edit(&mut self.filters);
        self.update_demand(cx);
        let filters = self.filters.clone();
        self.table.update(cx, |table, cx| {
            table.delegate_mut().set_filters(filters);
            cx.notify();
        });
        cx.notify();
    }

    pub fn clear_search_and_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.set_search(String::new(), cx);
        self.update_filters(cx, |f| {
            for kind in RuleKind::ALL {
                f.rule_mut(kind).enabled = false;
            }
            f.statuses.clear();
        });
    }

    pub fn set_view(&mut self, tree: bool, cx: &mut Context<Self>) {
        let view = if tree { "tree" } else { "flat" };
        self.monitor.update(cx, |monitor, cx| {
            monitor.save(json!({"behavior": {"processView": view}}), cx)
        });
    }

    fn toggle_column(&mut self, column: Column, cx: &mut Context<Self>) {
        self.monitor.update(cx, |monitor, cx| {
            let visible = monitor.prefs().column_visible(column.key()) == Some(true);
            monitor.save(
                json!({"appearance": {"columnVisibility": {column.key(): !visible}}}),
                cx,
            )
        });
    }

    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let monitor = self.monitor.read(cx);
        let paused = monitor.paused;
        let rate = monitor.refresh_rate();
        let current_theme = monitor.theme_name().to_owned();
        let (pause_monitor, rate_monitor, theme_monitor) = (
            self.monitor.clone(),
            self.monitor.clone(),
            self.monitor.clone(),
        );
        let status_color = if paused {
            cx.theme().muted_foreground
        } else {
            theme::palette(cx).running
        };
        TitleBar::new().child(
            h_flex()
                .w_full()
                .pr_3()
                // Leave room for the macOS window controls drawn over the title bar.
                .when(cfg!(target_os = "macos"), |this| this.pl(px(72.)))
                .justify_between()
                .child(
                    h_flex()
                        .gap_2()
                        .child(img(SMALL_ICON.clone()).size_4())
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Zentop"),
                        ),
                )
                .child(
                    h_flex()
                        .gap_1()
                        .child(
                            h_flex()
                                .mr_2()
                                .gap_1p5()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(div().size_1p5().rounded_full().bg(status_color))
                                .child(if paused {
                                    "Paused".to_string()
                                } else {
                                    format!("Live · {}s", sampling::interval(rate) as f64 / 1000.0)
                                }),
                        )
                        .child(
                            Button::new("pause")
                                .ghost()
                                .small()
                                .icon(if paused {
                                    IconName::Play
                                } else {
                                    IconName::Pause
                                })
                                .tooltip(if paused {
                                    "Resume updates"
                                } else {
                                    "Pause updates"
                                })
                                .on_click(move |_, _, cx| {
                                    pause_monitor.update(cx, |m, cx| m.set_paused(!paused, cx))
                                }),
                        )
                        .child(
                            Button::new("refresh-rate")
                                .ghost()
                                .small()
                                .icon(IconName::Timer)
                                .tooltip("Refresh rate")
                                .dropdown_menu(move |mut menu, _, _| {
                                    for ms in REFRESH_RATES {
                                        let monitor = rate_monitor.clone();
                                        menu = menu.item(
                                            PopupMenuItem::new(format!("Every {}s", ms / 1000))
                                                .checked(rate == ms)
                                                .on_click(move |_, _, cx| {
                                                    monitor.update(cx, |m, cx| {
                                                        m.set_refresh_rate(ms, cx)
                                                    })
                                                }),
                                        );
                                    }
                                    menu
                                }),
                        )
                        .child(
                            Button::new("theme")
                                .ghost()
                                .small()
                                .icon(IconName::Palette)
                                .tooltip("Theme")
                                .dropdown_menu(move |mut menu, _, _| {
                                    menu = menu.scrollable(true).max_h(px(420.));
                                    for (name, label) in theme::all() {
                                        let monitor = theme_monitor.clone();
                                        menu = menu.item(
                                            PopupMenuItem::new(label)
                                                .checked(name == current_theme)
                                                .on_click(move |_, _, cx| {
                                                    monitor
                                                        .update(cx, |m, cx| m.set_theme(name, cx))
                                                }),
                                        );
                                    }
                                    menu
                                }),
                        )
                        .child(
                            Button::new("more")
                                .ghost()
                                .small()
                                .icon(IconName::Ellipsis)
                                .tooltip("More")
                                .dropdown_menu(|menu, _, _| {
                                    menu.menu("Settings…", Box::new(OpenSettings))
                                        .menu("Menu-bar Overview", Box::new(ShowOverview))
                                        .separator()
                                        .menu("About Zentop", Box::new(OpenAbout))
                                }),
                        ),
                ),
        )
    }

    fn render_filters(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.filters.active_count();
        let entity = cx.entity();
        let thresholds = self.thresholds.clone();
        Popover::new("filters")
            .trigger(
                Button::new("filters-button")
                    .small()
                    .outline()
                    .icon(IconName::Funnel)
                    .label(if count > 0 {
                        format!("Filters · {count}")
                    } else {
                        "Filters".into()
                    }),
            )
            .content(move |_, _, cx| {
                let filters = entity.read(cx).filters.clone();
                let mut rules = v_flex().gap_2().w(px(320.)).p_1();
                for (i, kind) in RuleKind::ALL.into_iter().enumerate() {
                    let rule = *filters.rule(kind);
                    let toggle = entity.clone();
                    let cycle = entity.clone();
                    rules = rules.child(
                        h_flex()
                            .gap_2()
                            .child(
                                Checkbox::new(("rule", i))
                                    .label(kind.label())
                                    .checked(rule.enabled)
                                    .on_click(move |checked, _, cx| {
                                        let checked = *checked;
                                        toggle.update(cx, |this, cx| {
                                            this.update_filters(cx, |f| {
                                                f.rule_mut(kind).enabled = checked
                                            })
                                        });
                                    }),
                            )
                            .child(div().flex_1())
                            .child(
                                Button::new(("operator", i))
                                    .small()
                                    .outline()
                                    .label(rule.operator.symbol())
                                    .accessibility_label(format!("{} comparison", kind.label()))
                                    .on_click(move |_, _, cx| {
                                        cycle.update(cx, |this, cx| {
                                            this.update_filters(cx, |f| {
                                                let r = f.rule_mut(kind);
                                                r.operator = r.operator.next();
                                            })
                                        });
                                    }),
                            )
                            .child(
                                div().w(px(96.)).child(
                                    NumberInput::new(&thresholds[i])
                                        .small()
                                        .suffix(div().text_xs().child(kind.unit()))
                                        .accessibility_label(format!("{} threshold", kind.label())),
                                ),
                            ),
                    );
                }
                let mut statuses = h_flex().flex_wrap().gap_3();
                for (i, status) in STATUSES.into_iter().enumerate() {
                    let checked = filters.statuses.iter().any(|s| s == status);
                    let entity = entity.clone();
                    statuses = statuses.child(
                        Checkbox::new(("status", i))
                            .label(status)
                            .checked(checked)
                            .on_click(move |checked, _, cx| {
                                let checked = *checked;
                                entity.update(cx, |this, cx| {
                                    this.update_filters(cx, |f| {
                                        f.statuses.retain(|s| s != status);
                                        if checked {
                                            f.statuses.push(status.to_owned());
                                        }
                                    })
                                });
                            }),
                    );
                }
                v_flex()
                    .gap_3()
                    .child(rules)
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Status"),
                    )
                    .child(statuses)
            })
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let delegate = self.table.read(cx).delegate();
        let tree = delegate.tree;
        let expanded = !delegate.expanded.is_empty();
        let (matching, total, families) = (delegate.matching, delegate.total(), delegate.families);
        let searching = delegate.searching();
        let entity = cx.entity();
        let visible: Vec<Column> = delegate.columns().to_vec();
        h_flex()
            .gap_2()
            .px_4()
            .py_3()
            .child(
                div().flex_1().max_w(px(560.)).child(
                    Input::new(&self.search).small().cleanable(true).prefix(
                        Icon::new(IconName::Search)
                            .small()
                            .text_color(cx.theme().muted_foreground),
                    ),
                ),
            )
            .child(self.render_filters(cx))
            .child(
                Button::new("columns")
                    .small()
                    .outline()
                    .icon(IconName::Columns3)
                    .label("Columns")
                    .dropdown_menu(move |mut menu, _, _| {
                        for column in Column::ALL.into_iter().filter(|c| *c != Column::Name) {
                            let entity = entity.clone();
                            menu = menu.item(
                                PopupMenuItem::new(column.label())
                                    .checked(visible.contains(&column))
                                    .on_click(move |_, _, cx| {
                                        entity.update(cx, |this, cx| this.toggle_column(column, cx))
                                    }),
                            );
                        }
                        menu
                    }),
            )
            .child(
                h_flex()
                    .child(
                        Button::new("view-list")
                            .small()
                            .with_variant(if tree {
                                gpui_kit::component::button::ButtonVariant::Ghost
                            } else {
                                gpui_kit::component::button::ButtonVariant::Secondary
                            })
                            .icon(IconName::List)
                            .tooltip("List view")
                            .on_click(cx.listener(|this, _, _, cx| this.set_view(false, cx))),
                    )
                    .child(
                        Button::new("view-tree")
                            .small()
                            .with_variant(if tree {
                                gpui_kit::component::button::ButtonVariant::Secondary
                            } else {
                                gpui_kit::component::button::ButtonVariant::Ghost
                            })
                            .icon(IconName::ListTree)
                            .tooltip("Tree view")
                            .on_click(cx.listener(|this, _, _, cx| this.set_view(true, cx))),
                    ),
            )
            .when(tree && expanded && !searching, |this| {
                this.child(
                    Button::new("collapse")
                        .small()
                        .ghost()
                        .label("Collapse all")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.table.update(cx, |table, cx| {
                                table.delegate_mut().collapse_all();
                                cx.notify();
                            });
                        })),
                )
            })
            .child(div().flex_1())
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if tree {
                        format!(
                            "{families} {}",
                            if families == 1 { "family" } else { "families" }
                        )
                    } else if searching {
                        format!("{matching} of {total} processes")
                    } else {
                        format!("{total} processes")
                    }),
            )
    }
}

impl Render for Zentop {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let monitor = self.monitor.read(cx);
        let alert = monitor
            .error
            .clone()
            .or_else(|| monitor.port_error.clone())
            .or_else(|| monitor.prefs_error.clone());
        let frame = monitor.frame.clone();
        let history = monitor.history.clone();
        let unavailable = monitor.error.is_some();
        let danger = theme::palette(cx).danger;
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_title_bar(cx))
            .when_some(frame, |this, frame| {
                this.child(stats::render(
                    &stats::Readings {
                        stats: &frame.stats,
                        gpus: &frame.gpus,
                        history: &history,
                        unavailable,
                    },
                    cx,
                ))
            })
            .child(self.render_toolbar(cx))
            .when_some(alert, |this, alert: String| {
                this.child(
                    div()
                        .mx_4()
                        .mb_2()
                        .px_3()
                        .py_2()
                        .rounded(cx.theme().radius)
                        .border_1()
                        .border_color(danger)
                        .bg(danger.opacity(0.08))
                        .text_color(danger)
                        .text_sm()
                        .child(SharedString::from(alert)),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .mx_4()
                    .mb_4()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(cx.theme().border)
                    .overflow_hidden()
                    .child(DataTable::new(&self.table).stripe(false).bordered(false)),
            )
    }
}

fn visible_columns(prefs: &Preferences) -> Vec<Column> {
    Column::ALL
        .into_iter()
        .filter(|c| *c == Column::Name || prefs.column_visible(c.key()).unwrap_or(false))
        .collect()
}

/// The "default status filter" preference, as an initial status filter.
fn default_statuses(prefs: &Preferences) -> Vec<String> {
    match prefs.behavior("defaultStatusFilter").as_str() {
        Some("running") => vec!["Running".into()],
        Some("sleeping") => vec!["Sleeping".into()],
        Some("idle") => vec!["Idle".into()],
        Some("unknown") => vec!["Unknown".into()],
        _ => Vec::new(),
    }
}
