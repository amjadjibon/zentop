//! The main window: resource cards and the process table.
use super::monitor::{self, Monitor};
use super::process_view::{self, Column, Filters, RuleKind, STATUSES};
use super::table::ProcessTable;
use super::{details, stats, theme, OpenAbout, OpenSettings, ShowOverview, SMALL_ICON};
use crate::preferences::Preferences;
use crate::sampling;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState, NumberInput};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::table::{DataTable, TableEvent, TableState};
use gpui_kit::component::{
    h_flex, v_flex, ActiveTheme, Disableable, Icon, Sizable, Size, TitleBar,
};
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
                .placeholder("Search processes…")
                .clean_on_escape()
        });
        let defaults = Filters::default();
        let thresholds = RuleKind::ALL.map(|kind| {
            let value = defaults.rule(kind).value.to_string();
            cx.new(|cx| InputState::new(window, cx).default_value(value))
        });
        let mut subscriptions = vec![
            cx.observe(&monitor, |this, _, cx| this.sync(cx)),
            // Columns stretch to the window, so the list never leaves dead space.
            cx.observe_window_bounds(window, |this, window, cx| this.fit_columns(window, cx)),
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
        this.fit_columns(window, cx);
        this
    }

    fn fit_columns(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let width = f32::from(window.viewport_size().width);
        self.table.update(cx, |table, cx| {
            if table.delegate_mut().fit_width(width) {
                table.refresh(cx);
                cx.notify();
            }
        });
    }

    /// Pulls the latest frame and preferences from the shared model.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let monitor = self.monitor.read(cx);
        let frame = monitor.frame.clone();
        let columns = visible_columns(monitor.prefs());
        let tree = monitor.prefs().behavior("processView").as_str() == Some("tree");
        let per_page = monitor
            .prefs()
            .behavior("itemsPerPage")
            .as_u64()
            .unwrap_or(15) as usize;
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
            if table.delegate().per_page != per_page {
                table.delegate_mut().set_per_page(per_page);
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
        let (pause_monitor, theme_monitor) = (self.monitor.clone(), self.monitor.clone());
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
                            Button::new("overview")
                                .ghost()
                                .small()
                                .icon(IconName::PanelsTopLeft)
                                .tooltip("Menu-bar overview")
                                .on_click(|_, window, cx| {
                                    window.dispatch_action(Box::new(ShowOverview), cx)
                                }),
                        )
                        .child(div().mx_1().w(px(1.)).h(px(16.)).bg(cx.theme().border))
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
                    .outline()
                    .icon(IconName::SlidersHorizontal)
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
        let total = delegate.total();
        let searching = delegate.searching();
        let entity = cx.entity();
        let visible: Vec<Column> = delegate.columns().to_vec();
        let segment = |id: &'static str, icon: IconName, label: &'static str, active: bool| {
            Button::new(id)
                .with_variant(if active {
                    ButtonVariant::Secondary
                } else {
                    ButtonVariant::Ghost
                })
                .icon(icon)
                .label(label)
        };
        h_flex()
            .gap_3()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Processes"))
                    .child(
                        div()
                            .text_color(cx.theme().muted_foreground)
                            .child(total.to_string()),
                    ),
            )
            .child(
                div().w(px(360.)).child(
                    Input::new(&self.search).cleanable(true).prefix(
                        Icon::new(IconName::Search)
                            .small()
                            .text_color(cx.theme().muted_foreground),
                    ),
                ),
            )
            .child(
                Button::new("search-help")
                    .ghost()
                    .small()
                    .icon(IconName::CircleQuestionMark)
                    .tooltip(
                        "Search names, commands or PIDs. Separate terms with commas. \
                         Use :3000 or port:443 to find what owns a port.",
                    ),
            )
            .child(self.render_filters(cx))
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
                h_flex()
                    .p_0p5()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(
                        segment("view-list", IconName::List, "List", !tree)
                            .on_click(cx.listener(|this, _, _, cx| this.set_view(false, cx))),
                    )
                    .child(
                        segment("view-tree", IconName::ListTree, "Tree", tree)
                            .on_click(cx.listener(|this, _, _, cx| this.set_view(true, cx))),
                    ),
            )
            .child(
                Button::new("columns")
                    .ghost()
                    .icon(IconName::Columns3)
                    .label("View")
                    .dropdown_caret(true)
                    .dropdown_menu(move |mut menu, _, _| {
                        menu = menu.label("Columns");
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
    }

    fn set_page(&mut self, page: usize, cx: &mut Context<Self>) {
        self.table.update(cx, |table, cx| {
            table.delegate_mut().set_page(page);
            table.scroll_to_row(0, cx);
            cx.notify();
        });
        cx.notify();
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let delegate = self.table.read(cx).delegate();
        let (tree, results) = (delegate.tree, delegate.result_count());
        let (page, pages, per_page) = (delegate.page, delegate.page_count(), delegate.per_page);
        let monitor = self.monitor.clone();
        let noun = match (tree, results == 1) {
            (true, true) => "family",
            (true, false) => "families",
            (false, true) => "process",
            (false, false) => "processes",
        };
        let pager = |id: &'static str,
                     icon: IconName,
                     tooltip: &'static str,
                     target: usize,
                     enabled: bool| {
            Button::new(id)
                .small()
                .outline()
                .icon(icon)
                .tooltip(tooltip)
                .disabled(!enabled)
                .on_click(cx.listener(move |this, _, _, cx| this.set_page(target, cx)))
        };
        h_flex()
            .flex_shrink_0()
            .justify_between()
            .px_4()
            .py_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(format!("{results} {noun}"))
            .child(
                h_flex()
                    .gap_2()
                    .child("Rows")
                    .child(
                        Button::new("rows-per-page")
                            .small()
                            .outline()
                            .label(per_page.to_string())
                            .dropdown_caret(true)
                            .dropdown_menu(move |mut menu, _, _| {
                                for size in ITEMS_PER_PAGE {
                                    let monitor = monitor.clone();
                                    menu = menu.item(
                                        PopupMenuItem::new(size.to_string())
                                            .checked(size == per_page)
                                            .on_click(move |_, _, cx| {
                                                monitor.update(cx, |m, cx| {
                                                    m.save(
                                                        json!({"behavior": {"itemsPerPage": size}}),
                                                        cx,
                                                    )
                                                })
                                            }),
                                    );
                                }
                                menu
                            }),
                    )
                    .child(div().px_1().child(format!("Page {} of {pages}", page + 1)))
                    .child(pager(
                        "page-first",
                        IconName::ChevronsLeft,
                        "First page",
                        0,
                        page > 0,
                    ))
                    .child(pager(
                        "page-prev",
                        IconName::ChevronLeft,
                        "Previous page",
                        page.saturating_sub(1),
                        page > 0,
                    ))
                    .child(pager(
                        "page-next",
                        IconName::ChevronRight,
                        "Next page",
                        page + 1,
                        page + 1 < pages,
                    ))
                    .child(pager(
                        "page-last",
                        IconName::ChevronsRight,
                        "Last page",
                        pages - 1,
                        page + 1 < pages,
                    )),
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
                        .my_2()
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
                div().flex_1().min_h_0().child(
                    DataTable::new(&self.table)
                        .stripe(false)
                        .bordered(false)
                        .with_size(Size::Size(px(34.)))
                        .header_row_height(px(36.)),
                ),
            )
            .child(self.render_footer(cx))
    }
}

const ITEMS_PER_PAGE: [usize; 6] = [15, 25, 50, 100, 250, 500];

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
