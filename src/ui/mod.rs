//! The NeoHtop main window.
mod details;
mod process_view;
mod stats;
mod table;
pub mod theme;

use crate::preferences::Preferences;
use crate::sampling::{self, Frame, Sample, Snapshot};
use crate::state::AppState;
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
    SharedString, Styled, Subscription, Task, Window,
};
use process_view::{Column, Filters, RuleKind, STATUSES};
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};
use table::ProcessTable;

static APP_ICON: std::sync::LazyLock<Arc<gpui_kit::Image>> = std::sync::LazyLock::new(|| {
    Arc::new(gpui_kit::Image::from_bytes(
        gpui_kit::ImageFormat::Png,
        include_bytes!("../../assets/icons/32x32.png").to_vec(),
    ))
});

const REFRESH_RATES: [u64; 5] = [2000, 3000, 5000, 10000, 30000];
/// How often the loop checks whether a capture is due; captures stay interval-gated.
const TICK: Duration = Duration::from_millis(250);

pub struct NeoHtop {
    state: Arc<AppState>,
    prefs: Preferences,
    frame: Option<Arc<Frame>>,
    history: Vec<Sample>,
    paused: bool,
    search_text: String,
    filters: Filters,
    error: Option<String>,
    port_error: Option<String>,
    prefs_error: Option<String>,
    next_poll: Option<Instant>,
    sampling: bool,
    search: Entity<InputState>,
    thresholds: [Entity<InputState>; 4],
    table: Entity<TableState<ProcessTable>>,
    _subscriptions: Vec<Subscription>,
    _sampler: Task<()>,
}

impl NeoHtop {
    pub fn new(
        state: Arc<AppState>,
        prefs: Preferences,
        prefs_error: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let columns = visible_columns(&prefs);
        let tree = prefs.behavior("processView").as_str() == Some("tree");
        let root = cx.weak_entity();
        let table_state = state.clone();
        let table = cx.new(|cx| {
            TableState::new(
                ProcessTable::new(table_state, root, columns, tree),
                window,
                cx,
            )
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
            cx.subscribe_in(&search, window, |this, input, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().to_string();
                    this.set_search(text, cx);
                }
            }),
            cx.subscribe_in(
                &table,
                window,
                |this, table, event: &TableEvent, window, cx| {
                    if let TableEvent::DoubleClickedRow(row) = event {
                        if let Some(id) = table.read(cx).delegate().identity_at(*row) {
                            details::open(cx.entity(), id, window, cx);
                        }
                    }
                    let _ = this;
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

        let refresh = prefs.behavior("refreshRate").as_u64().unwrap_or(3000);
        if let Ok(mut hub) = state.hub.lock() {
            hub.interval = sampling::interval(refresh);
        }
        let sampler = cx.spawn(async move |this, cx| loop {
            let Ok(job) = this.update(cx, |this, _| this.poll_job()) else {
                break;
            };
            if let Some((state, gpu, ports)) = job {
                let result = cx
                    .background_executor()
                    .spawn(async move { sample(&state, gpu, ports) })
                    .await;
                if this.update(cx, |this, cx| this.apply(result, cx)).is_err() {
                    break;
                }
            }
            cx.background_executor().timer(TICK).await;
        });

        Self {
            state,
            prefs,
            frame: None,
            history: Vec::new(),
            paused: false,
            search_text: String::new(),
            filters: Filters::default(),
            error: None,
            port_error: None,
            prefs_error,
            next_poll: None,
            sampling: false,
            search,
            thresholds,
            table,
            _subscriptions: subscriptions,
            _sampler: sampler,
        }
    }

    pub fn state(&self) -> Arc<AppState> {
        self.state.clone()
    }

    pub fn frame(&self) -> Option<Arc<Frame>> {
        self.frame.clone()
    }

    /// Requests a capture on the next tick instead of waiting for the interval.
    pub fn refresh_now(&mut self, _: &mut Context<Self>) {
        self.next_poll = None;
    }

    fn poll_job(&mut self) -> Option<(Arc<AppState>, bool, bool)> {
        if self.sampling || self.next_poll.is_some_and(|due| Instant::now() < due) {
            return None;
        }
        let interval = self.state.hub.lock().map(|h| h.interval).unwrap_or(3000);
        self.next_poll = Some(Instant::now() + Duration::from_millis(interval));
        self.sampling = true;
        Some((self.state.clone(), self.gpu_details(), self.port_search()))
    }

    fn apply(&mut self, result: Result<Snapshot, String>, cx: &mut Context<Self>) {
        self.sampling = false;
        match result {
            Ok(snapshot) => {
                self.paused = snapshot.paused;
                self.error = snapshot.error;
                self.port_error = snapshot.port_error;
                self.history = snapshot.history;
                let frame = snapshot.frame;
                // A paused hub keeps returning the same frame; skip needless rebuilds.
                let changed = self
                    .frame
                    .as_ref()
                    .is_none_or(|old| old.sequence != frame.sequence);
                self.frame = Some(frame.clone());
                if changed || self.port_search() {
                    self.table.update(cx, |table, cx| {
                        table.delegate_mut().set_frame(frame);
                        // The visible range may not change when rows arrive.
                        let visible = table.visible_range().rows().clone();
                        let visible = visible.start..visible.end.max(visible.start + 40);
                        table.delegate_mut().load_icons(visible, cx);
                        cx.notify();
                    });
                }
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    fn gpu_details(&self) -> bool {
        self.prefs.column_visible(Column::Gpu.key()) == Some(true) || self.filters.gpu.enabled
    }

    fn port_search(&self) -> bool {
        process_view::has_port_query(&self.search_text)
    }

    fn set_search(&mut self, text: String, cx: &mut Context<Self>) {
        let was_port = self.port_search();
        self.search_text = text.clone();
        if !was_port && self.port_search() {
            crate::telemetry::track("port_search", None);
            self.refresh_now(cx);
        }
        self.table.update(cx, |table, cx| {
            table.delegate_mut().set_search(text);
            cx.notify();
        });
        cx.notify();
    }

    fn update_filters(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Filters)) {
        let gpu_before = self.gpu_details();
        edit(&mut self.filters);
        if self.gpu_details() != gpu_before {
            self.refresh_now(cx);
        }
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

    fn save(&mut self, patch: serde_json::Value, cx: &mut Context<Self>) {
        self.prefs_error = self.prefs.update(patch).err();
        cx.notify();
    }

    fn set_paused(&mut self, paused: bool, cx: &mut Context<Self>) {
        if let Ok(mut hub) = self.state.hub.lock() {
            hub.pause(paused);
        }
        self.paused = paused;
        self.refresh_now(cx);
        cx.notify();
    }

    fn set_refresh_rate(&mut self, ms: u64, cx: &mut Context<Self>) {
        if let Ok(mut hub) = self.state.hub.lock() {
            hub.interval = sampling::interval(ms);
        }
        self.next_poll = Some(Instant::now() + Duration::from_millis(sampling::interval(ms)));
        self.save(json!({"behavior": {"refreshRate": ms}}), cx);
    }

    fn set_view(&mut self, tree: bool, cx: &mut Context<Self>) {
        crate::telemetry::track(
            "process_view_changed",
            Some(if tree { "tree" } else { "flat" }),
        );
        self.table.update(cx, |table, cx| {
            table.delegate_mut().set_tree(tree);
            cx.notify();
        });
        self.save(
            json!({"behavior": {"processView": if tree { "tree" } else { "flat" }}}),
            cx,
        );
    }

    fn toggle_column(&mut self, column: Column, cx: &mut Context<Self>) {
        let visible = self.prefs.column_visible(column.key()) == Some(true);
        let gpu_before = self.gpu_details();
        self.save(
            json!({"appearance": {"columnVisibility": {column.key(): !visible}}}),
            cx,
        );
        if self.gpu_details() != gpu_before {
            self.refresh_now(cx);
        }
        let columns = visible_columns(&self.prefs);
        self.table.update(cx, |table, cx| {
            table.delegate_mut().set_columns(columns);
            table.refresh(cx);
            cx.notify();
        });
    }

    fn set_theme(&mut self, name: &str, cx: &mut Context<Self>) {
        theme::apply(name, cx);
        crate::telemetry::track("theme_changed", Some(name));
        self.save(json!({"appearance": {"theme": name}}), cx);
    }

    fn refresh_rate(&self) -> u64 {
        self.prefs.behavior("refreshRate").as_u64().unwrap_or(3000)
    }

    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let paused = self.paused;
        let rate = self.refresh_rate();
        let entity = cx.entity();
        let theme_entity = cx.entity();
        let current_theme = self
            .prefs
            .appearance("theme")
            .as_str()
            .unwrap_or(theme::DEFAULT_THEME)
            .to_owned();
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
                        .child(img(APP_ICON.clone()).size_4())
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("NeoHtop"),
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
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.set_paused(!paused, cx)),
                                ),
                        )
                        .child(
                            Button::new("refresh-rate")
                                .ghost()
                                .small()
                                .icon(IconName::Timer)
                                .tooltip("Refresh rate")
                                .dropdown_menu(move |mut menu, _, _| {
                                    for ms in REFRESH_RATES {
                                        let entity = entity.clone();
                                        menu = menu.item(
                                            PopupMenuItem::new(format!("Every {}s", ms / 1000))
                                                .checked(rate == ms)
                                                .on_click(move |_, _, cx| {
                                                    entity.update(cx, |this, cx| {
                                                        this.set_refresh_rate(ms, cx)
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
                                        let entity = theme_entity.clone();
                                        menu = menu.item(
                                            PopupMenuItem::new(label)
                                                .checked(name == current_theme)
                                                .on_click(move |_, _, cx| {
                                                    entity.update(cx, |this, cx| {
                                                        this.set_theme(name, cx)
                                                    })
                                                }),
                                        );
                                    }
                                    menu
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

impl Render for NeoHtop {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let alert = self
            .error
            .clone()
            .or_else(|| self.port_error.clone())
            .or_else(|| self.prefs_error.clone());
        let danger = theme::palette(cx).danger;
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_title_bar(cx))
            .when_some(self.frame.clone(), |this, frame| {
                this.child(stats::render(
                    &stats::Readings {
                        stats: &frame.stats,
                        gpus: &frame.gpus,
                        history: &self.history,
                        unavailable: self.error.is_some(),
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

/// One capture through the shared, interval-gated hub.
fn sample(state: &AppState, gpu: bool, ports: bool) -> Result<Snapshot, String> {
    let mut hub = state.hub.lock().map_err(|e| e.to_string())?;
    hub.process_gpu = gpu;
    let mut snapshot = hub.read(state)?;
    drop(hub);
    if ports {
        snapshot.attach_ports();
    }
    Ok(snapshot)
}

fn visible_columns(prefs: &Preferences) -> Vec<Column> {
    Column::ALL
        .into_iter()
        .filter(|c| *c == Column::Name || prefs.column_visible(c.key()).unwrap_or(false))
        .collect()
}
