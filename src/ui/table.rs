//! The process table: a virtualized `DataTable` over the latest frame.
use super::process_view::{self, identity, Column, Filters, Identity, Query, Sort, TreeRow};
use super::{details, theme, Zentop};
use crate::native_icons::{Icon, IconRequest, MAX_BATCH};
use crate::sampling::Frame;
use crate::state::AppState;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::table::{Column as TableColumn, ColumnSort, TableDelegate, TableState};
use gpui_kit::component::{h_flex, ActiveTheme, Disableable, Icon as UiIcon, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    div, img, px, App, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    Styled, WeakEntity, Window,
};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

const ACTIONS_WIDTH: f32 = 104.;

enum Row {
    Flat(usize),
    Tree(TreeRow),
}

impl Row {
    fn index(&self) -> usize {
        match self {
            Row::Flat(i) => *i,
            Row::Tree(row) => row.index,
        }
    }
    fn tree(&self) -> Option<&TreeRow> {
        match self {
            Row::Flat(_) => None,
            Row::Tree(row) => Some(row),
        }
    }
}

pub struct ProcessTable {
    state: Arc<AppState>,
    root: WeakEntity<Zentop>,
    frame: Option<Arc<Frame>>,
    search: String,
    filters: Filters,
    sort: Sort,
    pinned: HashSet<String>,
    pub tree: bool,
    pub expanded: HashSet<Identity>,
    columns: Vec<Column>,
    rows: Vec<Row>,
    /// Processes passing search and filters.
    pub matching: usize,
    /// Root families shown in tree view.
    pub families: usize,
    /// Zero-based page, and rows (or families in tree view) per page.
    pub page: usize,
    pub per_page: usize,
    /// Width the table has to fill; spare room goes to the text columns.
    available_width: f32,
    icons: HashMap<Identity, Option<Icon>>,
}

impl ProcessTable {
    pub fn new(
        state: Arc<AppState>,
        root: WeakEntity<Zentop>,
        columns: Vec<Column>,
        tree: bool,
    ) -> Self {
        Self {
            state,
            root,
            frame: None,
            search: String::new(),
            filters: Filters::default(),
            sort: Sort::default(),
            pinned: HashSet::new(),
            tree,
            expanded: HashSet::new(),
            columns,
            rows: Vec::new(),
            matching: 0,
            families: 0,
            page: 0,
            per_page: 15,
            available_width: 0.,
            icons: HashMap::new(),
        }
    }

    pub fn total(&self) -> usize {
        self.frame.as_ref().map_or(0, |f| f.processes.len())
    }

    pub fn searching(&self) -> bool {
        !self.search.trim().is_empty() || self.filters.active_count() > 0
    }

    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    pub fn set_frame(&mut self, frame: Arc<Frame>) {
        // Forget icons of exited processes so the cache tracks what is listed.
        let live: HashSet<Identity> = frame.processes.iter().map(identity).collect();
        self.icons.retain(|id, _| live.contains(id));
        self.expanded.retain(|id| live.contains(id));
        self.frame = Some(frame);
        self.rebuild();
    }

    pub fn set_search(&mut self, search: String) {
        self.search = search;
        self.page = 0;
        self.rebuild();
    }

    pub fn set_filters(&mut self, filters: Filters) {
        self.filters = filters;
        self.page = 0;
        self.rebuild();
    }

    pub fn set_columns(&mut self, columns: Vec<Column>) {
        self.columns = columns;
    }

    pub fn set_tree(&mut self, tree: bool) {
        self.tree = tree;
        self.page = 0;
        self.rebuild();
    }

    pub fn toggle_expanded(&mut self, id: Identity) {
        if !self.expanded.remove(&id) {
            self.expanded.insert(id);
        }
        self.rebuild();
    }

    pub fn collapse_all(&mut self) {
        self.expanded.clear();
        self.rebuild();
    }

    pub fn toggle_pin(&mut self, command: &str) {
        if !self.pinned.remove(command) {
            self.pinned.insert(command.to_owned());
        }
        self.rebuild();
    }

    fn rebuild(&mut self) {
        let Some(frame) = self.frame.clone() else {
            self.rows.clear();
            return;
        };
        let processes = &frame.processes;
        let query = Query::new(&self.search);
        let mut matched: Vec<usize> = (0..processes.len())
            .filter(|&i| process_view::matches(&processes[i], &query, &self.filters))
            .collect();
        self.matching = matched.len();
        if self.tree {
            let mut order: Vec<usize> = (0..processes.len()).collect();
            process_view::sort(&mut order, processes, self.sort, &self.pinned);
            let matching: Option<HashSet<usize>> =
                self.searching().then(|| matched.into_iter().collect());
            let (rows, families) = process_view::build_tree(
                processes,
                &order,
                &self.expanded,
                matching.as_ref(),
                self.sort,
                &self.pinned,
            );
            self.families = families;
            self.page = self.page.min(self.page_count() - 1);
            let first = self.page * self.per_page;
            let families = first..first + self.per_page;
            self.rows = rows
                .into_iter()
                .filter(|row| families.contains(&row.root_index))
                .map(Row::Tree)
                .collect();
        } else {
            process_view::sort(&mut matched, processes, self.sort, &self.pinned);
            self.families = 0;
            self.page = self.page.min(self.page_count() - 1);
            self.rows = matched
                .into_iter()
                .skip(self.page * self.per_page)
                .take(self.per_page)
                .map(Row::Flat)
                .collect();
        }
    }

    /// Resizes columns to fill `width`. Returns whether anything changed.
    pub fn fit_width(&mut self, width: f32) -> bool {
        let changed = (self.available_width - width).abs() >= 1.;
        self.available_width = width;
        changed
    }

    fn column_width(&self, column: Column) -> f32 {
        // Room for the actions column and the vertical scrollbar.
        let used: f32 = self.columns.iter().map(|c| c.width()).sum::<f32>() + ACTIONS_WIDTH + 16.;
        let spare = (self.available_width - used).max(0.);
        let growing: Vec<Column> = self
            .columns
            .iter()
            .copied()
            .filter(|c| {
                matches!(
                    c,
                    Column::Name | Column::Command | Column::Root | Column::Environ
                )
            })
            .collect();
        if growing.contains(&column) {
            column.width() + spare / growing.len() as f32
        } else {
            column.width()
        }
    }

    /// Processes, or process families in tree view, across all pages.
    pub fn result_count(&self) -> usize {
        if self.tree {
            self.families
        } else {
            self.matching
        }
    }

    pub fn page_count(&self) -> usize {
        self.result_count().div_ceil(self.per_page.max(1)).max(1)
    }

    pub fn set_page(&mut self, page: usize) {
        self.page = page.min(self.page_count() - 1);
        self.rebuild();
    }

    pub fn set_per_page(&mut self, per_page: usize) {
        if per_page != self.per_page {
            self.per_page = per_page.max(1);
            self.page = 0;
            self.rebuild();
        }
    }

    fn process(&self, row_ix: usize) -> Option<&crate::monitoring::ProcessInfo> {
        let frame = self.frame.as_ref()?;
        frame.processes.get(self.rows.get(row_ix)?.index())
    }

    pub fn identity_at(&self, row_ix: usize) -> Option<Identity> {
        self.process(row_ix).map(identity)
    }

    fn render_name(
        &self,
        row_ix: usize,
        cx: &mut Context<TableState<Self>>,
    ) -> gpui_kit::AnyElement {
        let Some(p) = self.process(row_ix) else {
            return div().into_any_element();
        };
        let id = identity(p);
        let tree = self.rows[row_ix].tree();
        let pinned = self.pinned.contains(&p.command);
        let searching = self.searching();
        let icon = self.icons.get(&id).cloned().flatten();
        h_flex()
            .gap_2()
            .min_w_0()
            .when_some(tree, |this, tree| {
                this.pl(px(tree.depth.min(10) as f32 * 16.))
                    .child(if tree.child_count > 0 {
                        Button::new(("expand", row_ix))
                            .ghost()
                            .xsmall()
                            .icon(if tree.expanded {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .disabled(searching)
                            .accessibility_label(format!(
                                "{} {} children",
                                if tree.expanded { "Collapse" } else { "Expand" },
                                p.name
                            ))
                            .when(searching, |b| {
                                b.tooltip("Clear search and filters to collapse this branch")
                            })
                            .on_click(cx.listener(move |table, _, _, cx| {
                                table.delegate_mut().toggle_expanded(id);
                                cx.notify();
                            }))
                            .into_any_element()
                    } else {
                        div().size_5().flex_shrink_0().into_any_element()
                    })
            })
            .child(match icon {
                Some(icon) => img(icon).size_4().flex_shrink_0().into_any_element(),
                // Background services and tools without an app icon.
                None => UiIcon::new(IconName::AppWindow)
                    .small()
                    .flex_shrink_0()
                    .text_color(cx.theme().muted_foreground)
                    .into_any_element(),
            })
            .child(div().truncate().child(p.name.clone()))
            .when_some(tree.filter(|t| t.family_count > 1), |this, tree| {
                this.child(
                    div()
                        .px_1()
                        .rounded_sm()
                        .text_xs()
                        .bg(cx.theme().secondary)
                        .text_color(cx.theme().muted_foreground)
                        .child(tree.family_count.to_string()),
                )
            })
            .when(pinned, |this| {
                this.child(
                    UiIcon::new(IconName::Pin)
                        .xsmall()
                        .text_color(cx.theme().primary),
                )
            })
            .into_any_element()
    }

    fn render_actions(
        &self,
        row_ix: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> gpui_kit::AnyElement {
        let _ = window;
        let Some(p) = self.process(row_ix) else {
            return div().into_any_element();
        };
        let id = identity(p);
        let command = p.command.clone();
        let pinned = self.pinned.contains(&command);
        let name = p.name.clone();
        h_flex()
            .gap_0p5()
            .child(
                Button::new(("pin", row_ix))
                    .ghost()
                    .xsmall()
                    .icon(if pinned {
                        IconName::PinOff
                    } else {
                        IconName::Pin
                    })
                    .tooltip(if pinned { "Unpin" } else { "Pin to top" })
                    .on_click(cx.listener(move |table, _, _, cx| {
                        table.delegate_mut().toggle_pin(&command);
                        cx.notify();
                    })),
            )
            .child(
                Button::new(("info", row_ix))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Info)
                    .tooltip("Details")
                    .on_click(move |_, window, cx| {
                        details::open(id, window, cx);
                    }),
            )
            .child(
                Button::new(("kill", row_ix))
                    .ghost()
                    .xsmall()
                    .icon(IconName::X)
                    .tooltip("End process")
                    .on_click(move |_, window, cx| {
                        details::confirm_kill(id, name.clone(), window, cx);
                    }),
            )
            .into_any_element()
    }

    pub fn load_icons(&mut self, range: Range<usize>, cx: &mut Context<TableState<Self>>) {
        let requests: Vec<IconRequest> = range
            .filter_map(|row| self.identity_at(row))
            .filter(|id| !self.icons.contains_key(id))
            .take(MAX_BATCH)
            .map(|(pid, start_time)| IconRequest { pid, start_time })
            .collect();
        if requests.is_empty() {
            return;
        }
        for request in &requests {
            self.icons.insert((request.pid, request.start_time), None);
        }
        let state = self.state.clone();
        cx.spawn(async move |table, cx| {
            let resolved = cx
                .background_executor()
                .spawn(async move { crate::native_icons::resolve_batch(&state, requests) })
                .await;
            let _ = table.update(cx, |table, cx| {
                for (id, icon) in resolved {
                    if icon.is_some() {
                        table.delegate_mut().icons.insert(id, icon);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl TableDelegate for ProcessTable {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len() + 1
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> TableColumn {
        let Some(&column) = self.columns.get(col_ix) else {
            return TableColumn::new("actions", "")
                .width(px(ACTIONS_WIDTH))
                .resizable(false)
                .movable(false);
        };
        let sort = if self.sort.column == column {
            if self.sort.ascending {
                ColumnSort::Ascending
            } else {
                ColumnSort::Descending
            }
        } else {
            ColumnSort::Default
        };
        let mut c = TableColumn::new(column.key(), column.label())
            .width(px(self.column_width(column)))
            .sort(sort)
            .movable(false);
        if column.numeric() {
            c = c.text_right();
        }
        if column == Column::Name {
            c = c.fixed_left();
        }
        c
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        let Some(&column) = self.columns.get(col_ix) else {
            return;
        };
        self.sort = match sort {
            ColumnSort::Default => Sort::default(),
            ColumnSort::Ascending => Sort {
                column,
                ascending: true,
            },
            ColumnSort::Descending => Sort {
                column,
                ascending: false,
            },
        };
        self.rebuild();
        // Re-read headers so the default sort's indicator is shown again.
        if sort == ColumnSort::Default {
            let table = cx.entity();
            cx.defer(move |cx| table.update(cx, |table, cx| table.refresh(cx)));
        }
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let numeric = self.columns.get(col_ix).is_some_and(|c| c.numeric());
        let actions = col_ix >= self.columns.len();
        h_flex()
            .size_full()
            .min_w_0()
            .items_center()
            .when(numeric, |this| this.justify_end())
            .when(actions, |this| this.justify_end().pr_2())
            .child(self.cell(row_ix, col_ix, window, cx))
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(&column) = self.columns.get(col_ix) else {
            return h_flex()
                .size_full()
                .justify_end()
                .pr_2()
                .text_color(cx.theme().foreground)
                .child("Actions");
        };
        // The sorted column is highlighted, as in earlier releases.
        let sorted = self.sort.column == column;
        h_flex()
            .size_full()
            .when(column.numeric(), |this| this.justify_end())
            .text_color(if sorted {
                cx.theme().primary
            } else {
                cx.theme().muted_foreground
            })
            .when(sorted, |this| {
                this.font_weight(gpui_kit::FontWeight::MEDIUM)
            })
            .child(column.label())
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, _: &App) -> String {
        match (self.process(row_ix), self.columns.get(col_ix)) {
            (Some(p), Some(column)) => column.text(p),
            _ => String::new(),
        }
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let pinned = self
            .process(row_ix)
            .is_some_and(|p| self.pinned.contains(&p.command));
        div()
            .id(("row", row_ix))
            .when(pinned, |d| d.bg(cx.theme().primary.opacity(0.06)))
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let searching = self.searching();
        let root = self.root.clone();
        h_flex()
            .size_full()
            .justify_center()
            .gap_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(if self.frame.is_none() {
                "Reading processes…"
            } else if searching {
                "No processes match the current search and filters."
            } else {
                "No processes."
            })
            .when(searching, |this| {
                this.child(
                    Button::new("clear-empty")
                        .small()
                        .label("Clear filters")
                        .on_click(move |_, window, cx| {
                            if let Some(root) = root.upgrade() {
                                root.update(cx, |root, cx| {
                                    root.clear_search_and_filters(window, cx)
                                });
                            }
                        }),
                )
            })
    }

    fn visible_rows_changed(
        &mut self,
        range: Range<usize>,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        self.load_icons(range, cx);
    }
}

impl ProcessTable {
    fn cell(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(&column) = self.columns.get(col_ix) else {
            return self.render_actions(row_ix, window, cx);
        };
        if column == Column::Name {
            return self.render_name(row_ix, cx);
        }
        let Some(p) = self.process(row_ix) else {
            return div().into_any_element();
        };
        let tree = self.rows[row_ix].tree();
        let palette = theme::palette(cx);
        let muted = cx.theme().muted_foreground;
        match column {
            Column::Cpu => {
                let cpu = tree.map_or(p.cpu_usage as f64, |t| t.family_cpu);
                div()
                    .when(cpu > 50.0, |d| {
                        d.px_1p5()
                            .rounded_sm()
                            .bg(palette.danger.opacity(0.12))
                            .text_color(palette.danger)
                    })
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_xs()
                    .child(format!("{cpu:.1}%"))
                    .into_any_element()
            }
            Column::Memory if tree.is_some() => div()
                .font_family(cx.theme().mono_font_family.clone())
                .text_xs()
                .child(process_view::format_mb(tree.map_or(0, |t| t.family_memory)))
                .into_any_element(),
            Column::Status => {
                let color = match p.status.as_str() {
                    "Running" => palette.running,
                    "Zombie" | "Stopped" => palette.stopped,
                    _ => palette.idle,
                };
                h_flex()
                    .gap_1p5()
                    .text_xs()
                    .text_color(muted)
                    .child(div().size_1p5().rounded_full().bg(color))
                    .child(p.status.clone())
                    .into_any_element()
            }
            _ => {
                let text: SharedString = column.text(p).into();
                div()
                    .truncate()
                    .when(column.numeric(), |d| {
                        d.font_family(cx.theme().mono_font_family.clone()).text_xs()
                    })
                    .when(
                        matches!(
                            column,
                            Column::Pid | Column::Ppid | Column::User | Column::Session
                        ),
                        |d| {
                            d.font_family(cx.theme().mono_font_family.clone())
                                .text_xs()
                                .text_color(muted)
                        },
                    )
                    .child(text)
                    .into_any_element()
            }
        }
    }
}
