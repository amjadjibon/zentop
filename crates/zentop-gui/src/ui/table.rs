//! The process list. Data columns fill the window and scroll sideways when
//! they do not fit; the Actions column stays pinned to the right edge, as in
//! earlier releases. (gpui-component tables can only pin columns to the left.)
use super::process_view::{self, identity, Column, Filters, Identity, Query, Sort, TreeRow};
use super::{details, theme, Zentop};
use crate::monitoring::ProcessInfo;
use crate::native_icons::{Icon, IconRequest, MAX_BATCH};
use crate::sampling::Frame;
use crate::state::AppState;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable, Icon as UiIcon, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    div, img, px, AnyElement, ClickEvent, Context, FontWeight, InteractiveElement, IntoElement,
    ParentElement, ScrollHandle, SharedString, StatefulInteractiveElement, Styled,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const ACTIONS_WIDTH: f32 = 112.;

type HoverHandler = Box<dyn Fn(&bool, &mut gpui_kit::Window, &mut gpui_kit::App)>;
type ClickHandler = Box<dyn Fn(&ClickEvent, &mut gpui_kit::Window, &mut gpui_kit::App)>;
const ROW_HEIGHT: f32 = 41.;
const HEADER_HEIGHT: f32 = 40.;

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

pub struct ProcessList {
    state: Arc<AppState>,
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
    /// Width the list has to fill; spare room goes to the text columns.
    available_width: f32,
    icons: HashMap<Identity, Option<Icon>>,
    /// Shared by the header and the rows so they scroll sideways together.
    horizontal: ScrollHandle,
    vertical: ScrollHandle,
    hovered: Option<usize>,
}

impl ProcessList {
    pub fn new(state: Arc<AppState>, columns: Vec<Column>, tree: bool) -> Self {
        Self {
            state,
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
            horizontal: ScrollHandle::new(),
            vertical: ScrollHandle::new(),
            hovered: None,
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

    pub fn collapse_all(&mut self) {
        self.expanded.clear();
        self.rebuild();
    }

    fn toggle_expanded(&mut self, id: Identity) {
        if !self.expanded.remove(&id) {
            self.expanded.insert(id);
        }
        self.rebuild();
    }

    fn toggle_pin(&mut self, command: &str) {
        if !self.pinned.remove(command) {
            self.pinned.insert(command.to_owned());
        }
        self.rebuild();
    }

    /// Clicking the sorted column flips its direction; another column starts
    /// with the largest values first (or A–Z for text).
    fn toggle_sort(&mut self, column: Column) {
        self.sort = if self.sort.column == column {
            Sort {
                column,
                ascending: !self.sort.ascending,
            }
        } else {
            let text = matches!(
                column,
                Column::Name
                    | Column::Status
                    | Column::User
                    | Column::Root
                    | Column::Command
                    | Column::Environ
            );
            Sort {
                column,
                ascending: text,
            }
        };
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

    /// Records the width the list has to fill.
    pub fn fit_width(&mut self, width: f32) {
        self.available_width = width;
    }

    /// Column widths. Every data column scales with the window in proportion
    /// to its natural width; only Actions is fixed. When the columns do not
    /// fit they keep their natural widths and scroll sideways under Actions.
    fn widths(&self) -> Vec<f32> {
        let natural: f32 = self.columns.iter().map(|c| c.width()).sum();
        let room = self.available_width - ACTIONS_WIDTH;
        let scale = if natural > 0. {
            (room / natural).max(1.)
        } else {
            1.
        };
        self.columns.iter().map(|c| c.width() * scale).collect()
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
        self.vertical.set_offset(Default::default());
    }

    pub fn set_per_page(&mut self, per_page: usize) {
        if per_page != self.per_page {
            self.per_page = per_page.max(1);
            self.page = 0;
            self.rebuild();
        }
    }

    fn process(&self, row_ix: usize) -> Option<&ProcessInfo> {
        let frame = self.frame.as_ref()?;
        frame.processes.get(self.rows.get(row_ix)?.index())
    }

    /// Resolves native icons for the rows on this page that do not have one yet.
    pub fn load_icons(&mut self, cx: &mut Context<Zentop>) {
        let requests: Vec<IconRequest> = (0..self.rows.len())
            .filter_map(|row| self.process(row).map(identity))
            .filter(|id| !self.icons.contains_key(id))
            .map(|(pid, start_time)| IconRequest { pid, start_time })
            .collect();
        if requests.is_empty() {
            return;
        }
        for request in &requests {
            self.icons.insert((request.pid, request.start_time), None);
        }
        let state = self.state.clone();
        cx.spawn(async move |this, cx| {
            for batch in requests.chunks(MAX_BATCH) {
                let (state, batch) = (state.clone(), batch.to_vec());
                let resolved = cx
                    .background_executor()
                    .spawn(async move { crate::native_icons::resolve_batch(&state, batch) })
                    .await;
                let updated = this.update(cx, |this, cx| {
                    this.list
                        .icons
                        .extend(resolved.into_iter().filter(|(_, icon)| icon.is_some()));
                    cx.notify();
                });
                if updated.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    pub fn render(&self, cx: &mut Context<Zentop>) -> AnyElement {
        let widths = self.widths();
        let content_width: f32 = widths.iter().sum();
        let border = cx.theme().border;
        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                h_flex()
                    .h(px(HEADER_HEIGHT))
                    .flex_shrink_0()
                    .bg(theme::palette(cx).card)
                    .border_b_1()
                    .border_color(border)
                    .child(
                        div()
                            .id("list-header")
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_x_scroll()
                            .track_scroll(&self.horizontal)
                            .child(h_flex().w(px(content_width)).h_full().children(
                                self.columns.iter().zip(&widths).enumerate().map(
                                    |(i, (&column, &width))| self.header_cell(i, column, width, cx),
                                ),
                            )),
                    )
                    .child(
                        h_flex()
                            .w(px(ACTIONS_WIDTH))
                            .h_full()
                            .flex_shrink_0()
                            .justify_center()
                            .border_l_1()
                            .border_color(border)
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child("Actions"),
                    ),
            )
            .child(if self.rows.is_empty() {
                self.render_empty(cx)
            } else {
                div()
                    .id("list-body")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.vertical)
                    .child(
                        h_flex()
                            .items_start()
                            .child(
                                div()
                                    .id("list-cells")
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_x_scroll()
                                    .track_scroll(&self.horizontal)
                                    .child(
                                        v_flex().w(px(content_width)).children(
                                            (0..self.rows.len())
                                                .map(|row| self.data_row(row, &widths, cx)),
                                        ),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .w(px(ACTIONS_WIDTH))
                                    .flex_shrink_0()
                                    .border_l_1()
                                    .border_color(border)
                                    .bg(theme::palette(cx).card)
                                    .children(
                                        (0..self.rows.len()).map(|row| self.actions_row(row, cx)),
                                    ),
                            ),
                    )
                    .vertical_scrollbar(&self.vertical)
                    // Sideways scrolling for columns that do not fit beside Actions.
                    .horizontal_scrollbar(&self.horizontal)
                    .into_any_element()
            })
            .into_any_element()
    }

    fn header_cell(
        &self,
        i: usize,
        column: Column,
        width: f32,
        cx: &mut Context<Zentop>,
    ) -> impl IntoElement {
        // The sorted column is highlighted, as in earlier releases.
        let sorted = self.sort.column == column;
        let icon = match (sorted, self.sort.ascending) {
            (true, true) => IconName::ArrowUp,
            (true, false) => IconName::ArrowDown,
            (false, _) => IconName::ChevronsUpDown,
        };
        h_flex()
            .id(("sort", i))
            .w(px(width))
            .h_full()
            .flex_shrink_0()
            .px_3()
            .gap_1()
            .cursor_pointer()
            .text_sm()
            .when(column.numeric(), |this| this.justify_end())
            .text_color(if sorted {
                cx.theme().primary
            } else {
                cx.theme().muted_foreground
            })
            .when(sorted, |this| this.font_weight(FontWeight::MEDIUM))
            .hover(|this| this.text_color(cx.theme().foreground))
            .child(div().truncate().child(column.label()))
            .child(
                UiIcon::new(icon)
                    .xsmall()
                    .when(!sorted, |icon| icon.opacity(0.5)),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.list.toggle_sort(column);
                cx.notify();
            }))
    }

    fn row_style<E: Styled + InteractiveElement>(
        &self,
        element: E,
        row: usize,
        cx: &Context<Zentop>,
    ) -> E {
        let pinned = self
            .process(row)
            .is_some_and(|p| self.pinned.contains(&p.command));
        let background = if pinned {
            Some(theme::mix(
                theme::palette(cx).card,
                cx.theme().primary,
                0.06,
            ))
        } else if self.hovered == Some(row) {
            Some(theme::mix(theme::palette(cx).card, cx.theme().muted, 0.5))
        } else {
            None
        };
        let element = element
            .h(px(ROW_HEIGHT))
            .border_b_1()
            .border_color(cx.theme().border.opacity(0.5));
        match background {
            Some(color) => element.bg(color),
            None => element,
        }
    }

    /// Hovering either half of a row highlights both; a double click opens details.
    fn hover_and_open(
        row: usize,
        id: Option<Identity>,
        cx: &mut Context<Zentop>,
    ) -> (HoverHandler, ClickHandler) {
        let hover: HoverHandler = Box::new(cx.listener(move |this, hovered: &bool, _, cx| {
            let next = if *hovered {
                Some(row)
            } else {
                this.list.hovered.filter(|r| *r != row)
            };
            if this.list.hovered != next {
                this.list.hovered = next;
                cx.notify();
            }
        }));
        let open: ClickHandler = Box::new(
            move |event: &ClickEvent, window: &mut gpui_kit::Window, cx: &mut gpui_kit::App| {
                if event.click_count() >= 2 {
                    if let Some(id) = id {
                        details::open(id, window, cx);
                    }
                }
            },
        );
        (hover, open)
    }

    fn data_row(&self, row: usize, widths: &[f32], cx: &mut Context<Zentop>) -> AnyElement {
        let id = self.process(row).map(identity);
        let (hover, open) = Self::hover_and_open(row, id, cx);
        let mut cells = h_flex().bg(theme::palette(cx).card);
        for (&column, &width) in self.columns.iter().zip(widths) {
            cells = cells.child(
                h_flex()
                    .w(px(width))
                    .h_full()
                    .flex_shrink_0()
                    .min_w_0()
                    .px_3()
                    .when(column.numeric(), |this| this.justify_end())
                    .child(self.cell(row, column, cx)),
            );
        }
        self.row_style(cells.id(("row", row)), row, cx)
            .on_hover(hover)
            .on_click(open)
            .into_any_element()
    }

    fn actions_row(&self, row: usize, cx: &mut Context<Zentop>) -> AnyElement {
        let Some(p) = self.process(row) else {
            return div().into_any_element();
        };
        let id = identity(p);
        let (hover, _) = Self::hover_and_open(row, Some(id), cx);
        let command = p.command.clone();
        let pinned = self.pinned.contains(&command);
        let name = p.name.clone();
        self.row_style(h_flex().id(("actions", row)), row, cx)
            .justify_center()
            .gap_0p5()
            .on_hover(hover)
            .child(
                Button::new(("pin", row))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Pin)
                    .when(pinned, |b| b.text_color(cx.theme().primary))
                    .tooltip(if pinned { "Unpin" } else { "Pin" })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.list.toggle_pin(&command);
                        cx.notify();
                    })),
            )
            .child(
                Button::new(("info", row))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Info)
                    .tooltip("Show details")
                    .on_click(move |_, window, cx| details::open(id, window, cx)),
            )
            .child(
                Button::new(("kill", row))
                    .ghost()
                    .xsmall()
                    .icon(IconName::X)
                    .tooltip("End process")
                    .on_click(move |_, window, cx| {
                        details::confirm_kill(id, name.clone(), window, cx)
                    }),
            )
            .into_any_element()
    }

    fn render_empty(&self, cx: &mut Context<Zentop>) -> AnyElement {
        let searching = self.searching();
        v_flex()
            .flex_1()
            .h(px(192.))
            .items_center()
            .justify_center()
            .gap_2()
            .text_sm()
            .child(
                UiIcon::new(IconName::Search)
                    .size_6()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(if self.frame.is_none() {
                "Reading processes…"
            } else if searching {
                "No matching processes"
            } else {
                "No processes in this snapshot"
            })
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if searching {
                        "Try another name, PID or condition."
                    } else {
                        "Wait for the next update or resume monitoring."
                    }),
            )
            .when(searching, |this| {
                this.child(
                    Button::new("clear-empty")
                        .link()
                        .small()
                        .label("Clear search and filters")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.clear_search_and_filters(window, cx)
                        })),
                )
            })
            .into_any_element()
    }

    fn render_name(&self, row: usize, p: &ProcessInfo, cx: &mut Context<Zentop>) -> AnyElement {
        let id = identity(p);
        let tree = self.rows[row].tree();
        let pinned = self.pinned.contains(&p.command);
        let searching = self.searching();
        let icon = self.icons.get(&id).cloned().flatten();
        h_flex()
            .gap_2()
            .min_w_0()
            .when_some(tree, |this, tree| {
                this.pl(px(tree.depth.min(10) as f32 * 16.))
                    .child(if tree.child_count > 0 {
                        Button::new(("expand", row))
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
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.list.toggle_expanded(id);
                                cx.notify();
                            }))
                            .into_any_element()
                    } else {
                        div().size_6().flex_shrink_0().into_any_element()
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
            .child(
                div()
                    .id(("process-name", row))
                    .min_w_0()
                    .truncate()
                    .text_size(px(13.))
                    .cursor_pointer()
                    .on_click(move |_, window, cx| details::open(id, window, cx))
                    .child(p.name.clone()),
            )
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

    fn cell(&self, row: usize, column: Column, cx: &mut Context<Zentop>) -> AnyElement {
        let Some(p) = self.process(row) else {
            return div().into_any_element();
        };
        if column == Column::Name {
            return self.render_name(row, p, cx);
        }
        let tree = self.rows[row].tree();
        let palette = theme::palette(cx);
        let muted = cx.theme().muted_foreground;
        let mono = cx.theme().mono_font_family.clone();
        match column {
            Column::Cpu => {
                let cpu = tree.map_or(p.cpu_usage as f64, |t| t.family_cpu);
                div()
                    .when(cpu > 50.0, |d| {
                        d.px_1p5()
                            .rounded_sm()
                            .py_1()
                            .bg(palette.danger.opacity(0.10))
                            .text_color(palette.danger)
                    })
                    .font_family(mono)
                    .text_xs()
                    .child(format!("{cpu:.1}%"))
                    .into_any_element()
            }
            Column::Memory if tree.is_some() => div()
                .font_family(mono)
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
                let dim = matches!(
                    column,
                    Column::Pid | Column::Ppid | Column::User | Column::Session
                );
                div()
                    .min_w_0()
                    .truncate()
                    .when(column.numeric() || dim, |d| d.font_family(mono).text_xs())
                    .when(!column.numeric() && !dim, |d| d.text_sm())
                    .when(dim, |d| d.text_color(muted))
                    .child(text)
                    .into_any_element()
            }
        }
    }
}
