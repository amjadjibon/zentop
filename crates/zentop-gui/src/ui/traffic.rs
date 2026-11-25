//! App-owned capture with a separate, dense HTTP-debugger window.
//!
//! - `bars`: title-bar toolbar, content-type tabs, filter bar and bottom bar.
//! - `setup`: the status pill's proxy-setup popover and capture control.
//! - `certificate`: certificate state and the exact-host decrypt list.
//! - `sidebar` and `navigation`: favorites and the Apps and Domains tree.
//! - `filter`, `table`: which rows show, and the virtual flow table.
//! - `detail`, `body`, `parse`: side-by-side request and response panes.
//! - `status`: throughput and Auto Select; `tools`: keys, HAR and replay.
mod bars;
mod body;
mod certificate;
mod detail;
mod filter;
mod navigation;
mod parse;
mod setup;
mod sidebar;
mod status;
mod table;
mod tools;

use detail::{Detail, PaneState, RequestTab, ResponseTab};
use filter::{RowFilter, Tabs};
use navigation::{Node, Pins, Scope, Sidebar};
use table::{Columns, Sort};

use super::{copy_button, monitor, process_view::Identity, theme, Windows};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{
    h_flex, v_flex, ActiveTheme, Disableable, Icon, Selectable, Sizable, TitleBar,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    div, px, size, uniform_list, AnyElement, App, AppContext, ClipboardItem, Context, Div, Entity,
    FocusHandle, Global, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, UniformListScrollHandle, Window,
};
use std::collections::HashSet;
use std::{sync::Arc, time::Duration};
use zentop_core::process_view::format_bytes;
use zentop_proxy::{
    Attribution, Body, Counters, Flow, FlowKind, FlowState, Headers, Metadata, RowUpdates, Session,
};

struct CaptureGlobal(Entity<Capture>);
impl Global for CaptureGlobal {}

pub(super) struct Capture {
    pub session: Option<Session>,
    decryption: Option<Arc<zentop_proxy::Decryption>>,
    certificates: certificate::CertificateSetup,
    generation: u64,
    _quit: Subscription,
}

pub(super) fn init(cx: &mut App) {
    let capture = cx.new(|cx: &mut Context<Capture>| Capture {
        session: None,
        decryption: None,
        certificates: Default::default(),
        generation: 0,
        _quit: cx.on_app_quit(|this, _| {
            this.session.take();
            async {}
        }),
    });
    cx.set_global(CaptureGlobal(capture));
}

pub(super) fn global(cx: &App) -> Entity<Capture> {
    cx.global::<CaptureGlobal>().0.clone()
}

fn stop(cx: &mut App) {
    global(cx).update(cx, |capture, cx| {
        capture.session.take();
        capture.decryption.take();
        capture.generation += 1;
        cx.notify();
    });
}

pub(super) fn overview(cx: &App) -> impl IntoElement {
    let capture = global(cx);
    let address = capture.read(cx).session.as_ref().map(Session::address);
    h_flex()
        .px_3()
        .gap_2()
        .child(
            Button::new("overview-traffic")
                .ghost()
                .small()
                .label("Traffic")
                .on_click(|_, _, cx| cx.defer(|cx| open(None, cx))),
        )
        .when_some(address, |row, address| {
            row.child(
                div()
                    .text_xs()
                    .flex_1()
                    .child(format!("Capturing · {address}")),
            )
            .child(
                Button::new("overview-stop-traffic")
                    .outline()
                    .small()
                    .label("Stop")
                    .on_click(|_, _, cx| stop(cx)),
            )
        })
}

pub(super) fn open(process: Option<Identity>, cx: &mut App) {
    super::companion::close(cx);
    certificate::load(&global(cx), cx);
    let existing = cx.global::<Windows>().traffic.clone();
    let focused = existing.is_some_and(|(handle, view)| {
        if !super::focus(Some(handle), cx) {
            return false;
        }
        if let Some(process) = process {
            let _ = view.update(cx, |this, cx| {
                this.process_scope = Some(process);
                cx.notify();
            });
        }
        true
    });
    if !focused {
        let options = super::window_options(
            size(px(1240.), px(820.)),
            Some(size(px(900.), px(600.))),
            cx,
        );
        if let Ok((handle, view)) = gpui_kit::open_window(options, cx, |window, cx| {
            window.set_window_title("Zentop Traffic");
            cx.new(|cx| Traffic::new(process, window, cx))
        }) {
            cx.global_mut::<Windows>().traffic = Some((handle, view.downgrade()));
        }
    }
    cx.activate(true);
}

/// Where the detail pane sits relative to the table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Placement {
    #[default]
    Below,
    Right,
    Hidden,
}

pub struct Traffic {
    capture: Entity<Capture>,
    generation: u64,
    revision: u64,
    rows: Vec<Metadata>,
    counters: Counters,
    // Navigation.
    sidebar: Sidebar,
    pins: Pins,
    expanded: HashSet<Node>,
    sidebar_filter: Entity<InputState>,
    show_sidebar: bool,
    scope: Option<Scope>,
    process_scope: Option<Identity>,
    // Filters.
    tabs: Tabs,
    show_filters: bool,
    filters: [Entity<InputState>; 4],
    status: Option<u16>,
    errors: bool,
    // Table.
    sort: Sort,
    columns: Columns,
    focus: FocusHandle,
    list_scroll: UniformListScrollHandle,
    auto_select: status::AutoSelect,
    // Detail.
    placement: Placement,
    selected: Option<u64>,
    detail: Option<Detail>,
    preview_task: Option<Task<()>>,
    request_tab: RequestTab,
    response_tab: ResponseTab,
    panes: [PaneState; 2],
    message_row: Option<usize>,
    reveal: bool,
    include_secrets: bool,
    // Proxy setup.
    setup_open: bool,
    error: Option<String>,
    allow_local: bool,
    port: Entity<InputState>,
    decrypt_input: Entity<InputState>,
    show_certificate: bool,
    exported_pem: Option<std::path::PathBuf>,
    // Status.
    include_bodies: bool,
    operation: Option<Result<String, String>>,
    throughput: status::Throughput,
    icons: std::collections::HashMap<Identity, Option<crate::native_icons::Icon>>,
    icon_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
    _tick: Task<()>,
}

impl Traffic {
    fn new(process_scope: Option<Identity>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let capture = global(cx);
        let prefs = monitor::global(cx);
        let saved_port = prefs.read(cx).prefs().behavior("trafficPort").as_u64();
        let pins = Pins::load(prefs.read(cx).prefs());
        let port = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("OS-assigned")
                .default_value(saved_port.map(|p| p.to_string()).unwrap_or_default())
        });
        let decrypt_input = cx.new(|cx| InputState::new(window, cx).placeholder("api.example.com"));
        let filters = ["Search URL, host, app", "App", "Host", "Method"]
            .map(|placeholder| cx.new(|cx| InputState::new(window, cx).placeholder(placeholder)));
        let sidebar_filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter"));
        let mut subscriptions = vec![cx.observe(&capture, |this, _, cx| this.refresh(cx))];
        for input in filters.iter().chain([&sidebar_filter]) {
            subscriptions.push(cx.subscribe_in(
                input,
                window,
                |_, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                },
            ));
        }
        let tick = cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;
            if this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                break;
            }
        });
        let mut this = Self {
            capture,
            generation: 0,
            revision: 0,
            rows: Vec::new(),
            counters: Counters::default(),
            sidebar: Sidebar::default(),
            pins,
            expanded: navigation::default_expanded(),
            sidebar_filter,
            show_sidebar: true,
            scope: None,
            process_scope,
            tabs: Tabs::default(),
            show_filters: false,
            filters,
            status: None,
            errors: false,
            sort: Sort::default(),
            columns: Columns::default(),
            focus: cx.focus_handle(),
            list_scroll: UniformListScrollHandle::new(),
            auto_select: Default::default(),
            placement: Placement::Below,
            selected: None,
            detail: None,
            preview_task: None,
            request_tab: RequestTab::default(),
            response_tab: ResponseTab::default(),
            panes: Default::default(),
            message_row: None,
            reveal: false,
            include_secrets: false,
            setup_open: false,
            error: None,
            allow_local: false,
            port,
            decrypt_input,
            show_certificate: false,
            exported_pem: None,
            include_bodies: false,
            operation: None,
            throughput: Default::default(),
            icons: Default::default(),
            icon_task: None,
            _subscriptions: subscriptions,
            _tick: tick,
        };
        this.focus.focus(window, cx);
        this.refresh(cx);
        this
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let capture = self.capture.read(cx);
        if self.generation != capture.generation {
            self.generation = capture.generation;
            self.revision = 0;
            self.rows.clear();
            self.sidebar = Sidebar::default();
            self.scope = None;
            self.operation = None;
            self.icon_task = None;
            self.icons.clear();
            self.select(None);
            self.allow_local = false;
            self.counters = Counters::default();
            self.throughput.clear();
            cx.notify();
        }
        if let Some(session) = &self.capture.read(cx).session {
            let counters = session.counters();
            let rates = self.throughput.rates();
            self.throughput
                .record(std::time::Instant::now(), counters.sent, counters.received);
            let changed = counters != self.counters || rates != self.throughput.rates();
            let rows = session.rows_since(&mut self.revision);
            if let Some(rows) = rows {
                match rows {
                    RowUpdates::Snapshot(rows) => {
                        self.sidebar = Sidebar::default();
                        for row in &rows {
                            self.sidebar.update(None, Some(row));
                        }
                        self.rows = rows;
                    }
                    RowUpdates::Changed(updates) => {
                        for (id, row) in updates {
                            let position = self.rows.binary_search_by(|row| id.cmp(&row.id));
                            self.sidebar
                                .update(position.ok().map(|index| &self.rows[index]), row.as_ref());
                            match (position, row) {
                                (Ok(index), Some(row)) => self.rows[index] = row,
                                (Ok(index), None) => {
                                    self.rows.remove(index);
                                }
                                (Err(index), Some(row)) => self.rows.insert(index, row),
                                (Err(_), None) => {}
                            }
                        }
                    }
                }
                if self
                    .selected
                    .is_some_and(|id| self.rows.iter().all(|row| row.id != id))
                {
                    self.select(None);
                }
                self.counters = counters;
                self.follow_newest(cx);
                self.refresh_detail(cx);
                cx.notify();
            } else if changed {
                // Throughput and memory change between row updates.
                self.counters = counters;
                cx.notify();
            }
        }
        self.resolve_icons(cx);
    }

    /// With Auto Select on, selects a newly arrived matching flow.
    fn follow_newest(&mut self, cx: &mut Context<Self>) {
        if !self.auto_select.enabled {
            return;
        }
        let newest = self
            .matching(cx)
            .iter()
            .map(|&index| self.rows[index].id)
            .max();
        if let Some(id) = self.auto_select.next(newest) {
            if self.selected != Some(id) {
                self.select(Some(id));
            }
            self.scroll_to(id, cx);
        }
    }

    fn select(&mut self, id: Option<u64>) {
        if self.selected == id {
            return;
        }
        self.selected = id;
        self.detail = None;
        self.preview_task = None;
        self.reveal = false;
        self.include_secrets = false;
        self.panes = Default::default();
        self.message_row = None;
    }

    fn resolve_icons(&mut self, cx: &mut Context<Self>) {
        if self.icon_task.is_some() {
            return;
        }
        if self.icons.len() > 2_000 {
            self.icons.retain(|id, _| {
                self.sidebar
                    .apps
                    .values()
                    .any(|app| app.identity == Some(*id))
            });
        }
        let mut ids = HashSet::new();
        let batch: Vec<_> = self
            .sidebar
            .apps
            .values()
            .filter_map(|app| app.identity)
            .filter(|id| !self.icons.contains_key(id) && ids.insert(*id))
            .take(crate::native_icons::MAX_BATCH)
            .map(|(pid, start_time)| crate::native_icons::IconRequest { pid, start_time })
            .collect();
        if batch.is_empty() {
            return;
        }
        let state = monitor::global(cx).read(cx).state();
        self.icon_task = Some(cx.spawn(async move |this, cx| {
            let icons = cx
                .background_executor()
                .spawn(async move {
                    let resolved = crate::native_icons::resolve_batch(&state, batch.clone());
                    let attempted = batch.into_iter().map(|r| ((r.pid, r.start_time), None));
                    attempted
                        .chain(resolved)
                        .collect::<std::collections::HashMap<_, _>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.icons.extend(icons);
                this.icon_task.take();
                cx.notify();
            });
        }));
    }

    fn row_filter(&self, cx: &App) -> RowFilter {
        let [search, process, host, method] = self
            .filters
            .each_ref()
            .map(|input| input.read(cx).value().trim().to_lowercase());
        RowFilter {
            search,
            process,
            host,
            method,
            status: self.status,
            errors: self.errors,
            tabs: self.tabs,
            scope: self.scope.clone(),
            process_scope: self.process_scope,
        }
    }

    /// Indices into `rows` that pass every filter, in table order.
    fn matching(&self, cx: &App) -> Vec<usize> {
        let filter = self.row_filter(cx);
        let mut matches: Vec<_> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| filter.matches(row))
            .map(|(index, _)| index)
            .collect();
        matches.sort_unstable_by(|&a, &b| self.sort.order(&self.rows[a], &self.rows[b]));
        matches
    }

    /// Clears the tabs, filter bar, sidebar scope and process scope.
    fn reset_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for input in &self.filters {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.tabs = Tabs::default();
        self.scope = None;
        self.process_scope = None;
        self.status = None;
        self.errors = false;
        cx.notify();
    }

    fn filters_active(&self, cx: &App) -> bool {
        !self.tabs.is_default()
            || self.scope.is_some()
            || self.process_scope.is_some()
            || self.status.is_some()
            || self.errors
            || self
                .filters
                .iter()
                .any(|input| !input.read(cx).value().trim().is_empty())
    }

    fn toggle_pin(&mut self, scope: &Scope, cx: &mut Context<Self>) {
        if !self.pins.toggle(scope) {
            return;
        }
        let monitor = monitor::global(cx);
        let patch = self.pins.patch();
        monitor.update(cx, |monitor, cx| monitor.save(patch, cx));
        if let Some(error) = monitor.read(cx).prefs_error.clone() {
            self.operation = Some(Err(error));
        }
        cx.notify();
    }
}

impl Render for Traffic {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_kit::component::resizable::{h_resizable, resizable_panel, v_resizable};
        let matches: Arc<[usize]> = self.matching(cx).into();
        let visible = matches.len();
        let selected_visible = self
            .selected
            .is_some_and(|id| matches.iter().any(|&index| self.rows[index].id == id));
        let table = self.table(matches, cx).into_any_element();
        let panes = match (self.placement, &self.detail) {
            (Placement::Hidden, _) | (_, None) => table,
            (Placement::Below, Some(detail)) => v_resizable("traffic-split")
                .child(
                    resizable_panel()
                        .size(px(250.))
                        .size_range(px(96.)..gpui_kit::Pixels::MAX)
                        .child(table),
                )
                .child(
                    resizable_panel()
                        .size_range(px(200.)..gpui_kit::Pixels::MAX)
                        .child(self.detail(detail, cx)),
                )
                .into_any_element(),
            (Placement::Right, Some(detail)) => h_resizable("traffic-split-right")
                .child(
                    resizable_panel()
                        .size(px(480.))
                        .size_range(px(280.)..gpui_kit::Pixels::MAX)
                        .child(table),
                )
                .child(
                    resizable_panel()
                        .size_range(px(320.)..gpui_kit::Pixels::MAX)
                        .child(self.detail(detail, cx)),
                )
                .into_any_element(),
        };
        v_flex()
            .id("traffic-window")
            .capture_key_down(cx.listener(Self::key))
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.title_bar(cx))
            .child(
                h_flex()
                    .items_stretch()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .when(self.show_sidebar, |row| row.child(self.sidebar(cx)))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .child(self.tab_strip(cx))
                            .when(self.show_filters, |column| {
                                column.child(self.filter_bar(cx))
                            })
                            .child(v_flex().flex_1().min_h_0().child(panes))
                            .child(self.bottom_bar(visible, selected_visible, cx)),
                    ),
            )
    }
}

fn identity(owner: &Attribution) -> Option<Identity> {
    match owner {
        Attribution::Known {
            pid, start_time, ..
        } => Some((*pid, *start_time)),
        Attribution::Unknown(_) => None,
    }
}

fn copy_line(id: &'static str, label: &'static str, value: String, cx: &App) -> Div {
    h_flex()
        .min_w_0()
        .gap_2()
        .child(
            div()
                .w(px(72.))
                .flex_shrink_0()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(
            div()
                .text_xs()
                .font_family(cx.theme().mono_font_family.clone())
                .min_w_0()
                .flex_1()
                .truncate()
                .child(value.clone()),
        )
        .child(div().flex_shrink_0().child(copy_button(id, label, value)))
}
