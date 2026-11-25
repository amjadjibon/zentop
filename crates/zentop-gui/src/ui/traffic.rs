//! App-owned capture with a separate virtualized traffic window.
mod certificate;
mod navigation;
mod tools;

use navigation::{Column, Scope, Sidebar, Sort};

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
use std::{sync::Arc, time::Duration};
use zentop_core::process_view::format_bytes;
use zentop_proxy::{
    Attribution, Body, Counters, Flow, FlowKind, FlowState, Headers, Metadata, Options, RowUpdates,
    Session,
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
            size(px(1160.), px(800.)),
            Some(size(px(890.), px(650.))),
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

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Tab {
    #[default]
    Request,
    Response,
    Timing,
    Messages,
}

impl Tab {
    fn name(self) -> &'static str {
        match self {
            Self::Request => "Request",
            Self::Response => "Response",
            Self::Timing => "Timing",
            Self::Messages => "Messages",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct DetailKey {
    id: u64,
    revision: u64,
    reveal: bool,
}

struct Detail {
    key: DetailKey,
    flow: Arc<Flow>,
    previews: Option<[Preview; 2]>,
}

enum Preview {
    Note(&'static str),
    Decoded {
        lines: Arc<[SharedString]>,
        binary: bool,
        limited: bool,
    },
    Error(String),
}

impl Preview {
    fn build(body: &Body, headers: &Headers) -> Self {
        if let Body::Skipped(reason) = body {
            return Self::Note(reason);
        }
        if body.kept().is_empty() {
            return Self::Note("Empty body");
        }
        match zentop_proxy::preview(body, headers) {
            Ok(preview) => Self::Decoded {
                lines: preview
                    .text
                    .lines()
                    .map(|line| {
                        const MAX_LINE_CHARS: usize = 2_000;
                        if let Some((end, _)) = line.char_indices().nth(MAX_LINE_CHARS) {
                            format!("{}…", &line[..end]).into()
                        } else {
                            line.into()
                        }
                    })
                    .collect(),
                binary: preview.binary,
                limited: preview.limited,
            },
            Err(error) => Self::Error(error.to_string()),
        }
    }
}

pub struct Traffic {
    capture: Entity<Capture>,
    generation: u64,
    revision: u64,
    rows: Vec<Metadata>,
    sidebar: Sidebar,
    show_sidebar: bool,
    scope: Option<Scope>,
    sort: Sort,
    focus: FocusHandle,
    list_scroll: UniformListScrollHandle,
    include_bodies: bool,
    message_row: Option<usize>,
    operation: Option<Result<String, String>>,
    selected: Option<u64>,
    detail_open: bool,
    detail: Option<Detail>,
    preview_task: Option<Task<()>>,
    tab: Tab,
    reveal: bool,
    include_secrets: bool,
    error: Option<String>,
    allow_local: bool,
    port: Entity<InputState>,
    filters: [Entity<InputState>; 4],
    decrypt_input: Entity<InputState>,
    show_certificate: bool,
    exported_pem: Option<std::path::PathBuf>,
    icons: std::collections::HashMap<Identity, Option<crate::native_icons::Icon>>,
    icon_task: Option<Task<()>>,
    status: Option<u16>,
    kind: Option<FlowKind>,
    errors: bool,
    process_scope: Option<Identity>,
    counters: Counters,
    _subscriptions: Vec<Subscription>,
    _tick: Task<()>,
}

impl Traffic {
    fn new(process_scope: Option<Identity>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let capture = global(cx);
        let saved_port = monitor::global(cx)
            .read(cx)
            .prefs()
            .behavior("trafficPort")
            .as_u64();
        let port = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("OS-assigned")
                .default_value(saved_port.map(|p| p.to_string()).unwrap_or_default())
        });
        let decrypt_input = cx.new(|cx| InputState::new(window, cx).placeholder("api.example.com"));
        let filters = ["Search traffic", "Process", "Host", "Method"]
            .map(|placeholder| cx.new(|cx| InputState::new(window, cx).placeholder(placeholder)));
        let mut subscriptions = vec![cx.observe(&capture, |this, _, cx| this.refresh(cx))];
        for input in &filters {
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
            sidebar: Sidebar::default(),
            show_sidebar: true,
            scope: None,
            sort: Sort::default(),
            focus: cx.focus_handle(),
            list_scroll: UniformListScrollHandle::new(),
            include_bodies: false,
            message_row: None,
            operation: None,
            selected: None,
            detail_open: false,
            detail: None,
            preview_task: None,
            tab: Tab::Request,
            reveal: false,
            include_secrets: false,
            error: None,
            allow_local: false,
            port,
            filters,
            decrypt_input,
            show_certificate: false,
            exported_pem: None,
            icons: Default::default(),
            icon_task: None,
            status: None,
            kind: None,
            errors: false,
            process_scope,
            counters: Counters::default(),
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
            cx.notify();
        }
        if let Some(session) = &self.capture.read(cx).session {
            let counters = session.counters();
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
                self.refresh_detail(cx);
                cx.notify();
            } else if counters != self.counters {
                self.counters = counters;
                cx.notify();
            }
        }
        self.resolve_icons(cx);
    }

    fn select(&mut self, id: Option<u64>) {
        self.selected = id;
        self.detail_open = id.is_some();
        self.detail = None;
        self.preview_task = None;
        self.reveal = false;
        self.include_secrets = false;
        self.tab = Tab::Request;
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
        let mut ids = std::collections::HashSet::new();
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

    fn refresh_detail(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.selected.filter(|_| self.detail_open) else {
            return;
        };
        let reveal = self.reveal;
        let revision = self.detail.as_ref().and_then(|detail| {
            (detail.key.id == id && detail.key.reveal == reveal).then_some(detail.key.revision)
        });
        let Some((revision, flow)) = self
            .capture
            .read(cx)
            .session
            .as_ref()
            .and_then(|session| session.flow_since(id, reveal, revision))
        else {
            return;
        };
        let key = DetailKey {
            id,
            revision,
            reveal,
        };
        let flow = Arc::new(flow);
        if flow.metadata.kind == FlowKind::Upgrade {
            let previews = Some([
                Preview::build(&flow.request_body, &flow.request.headers),
                Preview::build(&flow.response_body, &Vec::new()),
            ]);
            self.detail = Some(Detail {
                key,
                flow,
                previews,
            });
            return;
        }
        self.detail = Some(Detail {
            key,
            flow: flow.clone(),
            previews: None,
        });
        self.preview_task = Some(cx.spawn(async move |this, cx| {
            let previews = cx
                .background_executor()
                .spawn(async move {
                    let response = flow.response.as_ref().map(|head| &head.headers);
                    [
                        Preview::build(&flow.request_body, &flow.request.headers),
                        response.map_or(Preview::Note("No response headers"), |headers| {
                            Preview::build(&flow.response_body, headers)
                        }),
                    ]
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(detail) = this.detail.as_mut().filter(|detail| detail.key == key) {
                    detail.previews = Some(previews);
                    cx.notify();
                }
            });
        }));
    }

    fn configured_port(&self, cx: &App) -> Result<Option<u16>, &'static str> {
        let value = self.port.read(cx).value();
        if value.trim().is_empty() {
            return Ok(None);
        }
        value
            .trim()
            .parse()
            .ok()
            .filter(|&port| port != 0)
            .map(Some)
            .ok_or("Enter a port between 1 and 65535, or leave it blank.")
    }

    fn save_port(&mut self, cx: &mut Context<Self>) {
        match self.configured_port(cx) {
            Ok(port) => {
                let monitor = monitor::global(cx);
                monitor.update(cx, |monitor, cx| {
                    monitor.save(serde_json::json!({"behavior":{"trafficPort":port}}), cx)
                });
                self.error = monitor.read(cx).prefs_error.clone();
            }
            Err(error) => self.error = Some(error.into()),
        }
        cx.notify();
    }

    fn start(&mut self, port: Option<u16>, cx: &mut Context<Self>) {
        let options = Options {
            port,
            allow_local_destinations: self.allow_local,
        };
        let decryption = self
            .capture
            .read(cx)
            .certificates
            .bind(certificate::hosts(cx));
        let (decryption, session, error) = match decryption {
            Ok(decryption) => {
                let session = Session::start_with_decryption(options, decryption.clone());
                (Some(decryption), session, None)
            }
            Err(error) => (
                None,
                Session::start(options),
                Some(format!("Decryption unavailable: {error}")),
            ),
        };
        match session {
            Ok(session) => {
                self.error = error;
                self.capture.update(cx, |capture, cx| {
                    capture.session = Some(session);
                    capture.decryption = decryption;
                    cx.notify();
                });
            }
            Err(error) => self.error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn command(&mut self, command: impl FnOnce(&Session) -> bool, cx: &mut Context<Self>) -> bool {
        let sent = self.capture.read(cx).session.as_ref().is_some_and(command);
        self.error = (!sent).then(|| "Capture is busy. Try again.".into());
        cx.notify();
        sent
    }

    fn header(&self, cx: &mut Context<Self>) -> Div {
        let running = self.capture.read(cx).session.is_some();
        let address = self.capture.read(cx).session.as_ref().map(|session| {
            (
                session.address().to_string(),
                session.token().to_owned(),
                session.proxy_url(),
            )
        });
        let allow_local = if running {
            self.counters.allow_local_destinations
        } else {
            self.allow_local
        };
        let header = v_flex()
            .gap_3()
            .p_4()
            .bg(theme::palette(cx).card)
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        Button::new("traffic-start-stop")
                            .primary()
                            .small()
                            .label(if running { "Stop" } else { "Start" })
                            .icon(if running {
                                IconName::Square
                            } else {
                                IconName::Play
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if running {
                                    stop(cx);
                                } else {
                                    match this.configured_port(cx) {
                                        Ok(port) => this.start(port, cx),
                                        Err(error) => {
                                            this.error = Some(error.into());
                                            cx.notify();
                                        }
                                    }
                                }
                            })),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(if running {
                                "Capturing traffic"
                            } else {
                                "Capture stopped"
                            }),
                    )
                    .child(div().flex_1())
                    .child(
                        Checkbox::new("traffic-allow-local")
                            .checked(allow_local)
                            .label("Allow local destinations")
                            .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                if !running
                                    || this.command(
                                        |session| session.allow_local_destinations(*checked),
                                        cx,
                                    )
                                {
                                    this.allow_local = *checked;
                                    cx.notify();
                                }
                            })),
                    ),
            );
        header
            .when_some(address, |header, (address, token, proxy)| {
                header
                    .child(
                        h_flex()
                            .gap_5()
                            .child(copy_line("traffic-address", "Address", address, cx).w(px(245.)))
                            .child(copy_line("traffic-token", "Token", token, cx).flex_1()),
                    )
                    .child(
                        v_flex()
                            .gap_0p5()
                            .p_2()
                            .rounded_md()
                            .bg(cx.theme().background)
                            .child(copy_line(
                                "traffic-route-http",
                                "HTTP",
                                format!("export http_proxy={proxy}"),
                                cx,
                            ))
                            .child(copy_line(
                                "traffic-route-https",
                                "HTTPS",
                                "export https_proxy=$http_proxy".into(),
                                cx,
                            ))
                            .child(copy_line(
                                "traffic-route-curl",
                                "Try it",
                                "curl -x \"$http_proxy\" https://example.com".into(),
                                cx,
                            )),
                    )
            })
            .when(!running, |header| {
                header.child(
                    h_flex()
                        .gap_2()
                        .child(div().text_sm().child("Fixed port"))
                        .child(div().w(px(125.)).child(Input::new(&self.port).small()))
                        .child(
                            Button::new("traffic-save-port")
                                .outline()
                                .small()
                                .label("Save port")
                                .on_click(cx.listener(|this, _, _, cx| this.save_port(cx))),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("Leave blank for an OS-assigned port."),
                        ),
                )
            })
            .when_some(self.error.clone(), |header, error| {
                header.child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme::palette(cx).danger)
                                .child(error),
                        )
                        .when(
                            !running && self.configured_port(cx).is_ok_and(|port| port.is_some()),
                            |row| {
                                row.child(
                                    Button::new("traffic-auto-port")
                                        .outline()
                                        .small()
                                        .label("Start with OS-assigned port")
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.start(None, cx)),
                                        ),
                                )
                            },
                        ),
                )
            })
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> Div {
        let running = self.capture.read(cx).session.is_some();
        let paused = self.counters.paused;
        let status = self.status;
        let kind = self.kind;
        let entity = cx.entity();
        let kind_entity = entity.clone();
        let [search, process, host, method] = &self.filters;
        v_flex().gap_2().p_3().border_b_1().border_color(cx.theme().border)
            .child(h_flex().gap_2()
                .child(div().flex_1().min_w(px(115.)).child(Input::new(search).small()))
                .child(div().w(px(115.)).child(Input::new(process).small()))
                .child(div().w(px(115.)).child(Input::new(host).small()))
                .child(div().w(px(80.)).child(Input::new(method).small()))
                .child(Button::new("traffic-status-filter").outline().small()
                    .label(status.map_or_else(|| "Any status".into(), |class| format!("{class}xx")))
                    .dropdown_caret(true).dropdown_menu(move |mut menu, _, _| {
                        for class in [None, Some(1), Some(2), Some(3), Some(4), Some(5)] {
                            let entity = entity.clone();
                            menu = menu.item(PopupMenuItem::new(class.map_or_else(|| "Any status".into(), |n| format!("{n}xx")))
                                .checked(status == class).on_click(move |_, _, cx| entity.update(cx, |this, cx| {
                                    this.status = class;
                                    cx.notify();
                                })));
                        }
                        menu
                    }))
                .child(Button::new("traffic-kind-filter").outline().small()
                    .label(kind_name(kind)).dropdown_caret(true).dropdown_menu(move |mut menu, _, _| {
                        for option in [None, Some(FlowKind::Http), Some(FlowKind::Tunnel), Some(FlowKind::DecryptedHttps), Some(FlowKind::Upgrade)] {
                            let entity = kind_entity.clone();
                            menu = menu.item(PopupMenuItem::new(kind_name(option)).checked(kind == option)
                                .on_click(move |_, _, cx| entity.update(cx, |this, cx| {
                                    this.kind = option;
                                    cx.notify();
                                })));
                        }
                        menu
                    }))
                .child(Checkbox::new("traffic-error-filter").checked(self.errors).label("Has error")
                    .on_click(cx.listener(|this, value: &bool, _, cx| {
                        this.errors = *value;
                        cx.notify();
                    }))))
            .child(h_flex().gap_2()
                .child(Button::new("traffic-har-selected").outline().small().label("Export selected HAR")
                    .disabled(self.selected.is_none())
                    .on_click(cx.listener(|this, _, _, cx| this.export_har(true, cx))))
                .child(Button::new("traffic-har-filtered").outline().small().label("Export filtered HAR")
                    .disabled(self.rows.is_empty())
                    .on_click(cx.listener(|this, _, _, cx| this.export_har(false, cx))))
                .child(Checkbox::new("traffic-har-bodies").checked(self.include_bodies).label("Include bodies")
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.include_bodies = *checked;
                        cx.notify();
                    })))
                .child(div().text_xs().text_color(cx.theme().muted_foreground)
                    .child("HAR files contain captured traffic.")))
            .when_some(self.operation.as_ref(), |bar, result| {
                bar.child(div().text_xs().truncate().text_color(if result.is_ok() {
                    cx.theme().muted_foreground
                } else {
                    theme::palette(cx).danger
                }).child(match result { Ok(message) | Err(message) => message.clone() }))
            })
            .child(h_flex().gap_2()
                .child(Button::new("traffic-sidebar-toggle").ghost().small()
                    .label(if self.show_sidebar { "Hide sidebar" } else { "Show sidebar" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_sidebar = !this.show_sidebar;
                        cx.notify();
                    })))
                .child(Button::new("traffic-pause").outline().small().disabled(!running)
                    .label(if paused { "Resume" } else { "Pause" })
                    .on_click(cx.listener(move |this, _, _, cx| { this.command(|session| session.pause(!paused), cx); })))
                .child(Button::new("traffic-clear").ghost().small().disabled(!running).label("Clear")
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.command(Session::clear, cx) {
                            this.select(None);
                        }
                    })))
                .when_some(self.process_scope, |row, id| row.child(Button::new("traffic-process-scope")
                    .outline().small().label(format!("PID {} ×", id.0))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.process_scope = None;
                        cx.notify();
                    }))))
                .child(div().flex_1())
                .child(div().text_xs().text_color(cx.theme().muted_foreground).child(format!(
                    "Flows {}  ·  Refused {}  ·  Flows dropped {}  ·  Unattributed flows {}  ·  Bodies evicted {}",
                    self.rows.len(), self.counters.refused, self.counters.flows_dropped,
                    self.counters.unattributed, self.counters.evicted_bodies,
                ))))
    }

    fn matching(&self, cx: &App) -> Vec<usize> {
        let [search, process, host, method] = self
            .filters
            .each_ref()
            .map(|input| input.read(cx).value().trim().to_ascii_lowercase());
        let mut matches: Vec<_> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                let process_name = row.process.name().to_ascii_lowercase();
                let hostname = row.host.to_ascii_lowercase();
                let verb = row.method.to_ascii_lowercase();
                self.process_scope
                    .is_none_or(|id| identity(&row.process) == Some(id))
                    && self.scope.as_ref().is_none_or(|scope| scope.matches(row))
                    && process_name.contains(&process)
                    && hostname.contains(&host)
                    && verb.contains(&method)
                    && self
                        .status
                        .is_none_or(|class| row.status.is_some_and(|n| n / 100 == class))
                    && self.kind.is_none_or(|kind| row.kind == kind)
                    && (!self.errors
                        || row.state.has_error()
                        || row.status.is_some_and(|n| n >= 400))
                    && (search.is_empty()
                        || process_name.contains(&search)
                        || hostname.contains(&search)
                        || verb.contains(&search)
                        || row.path.to_ascii_lowercase().contains(&search))
            })
            .map(|(index, _)| index)
            .collect();
        matches.sort_unstable_by(|&a, &b| self.sort.order(&self.rows[a], &self.rows[b]));
        matches
    }

    fn table(&self, cx: &Context<Self>) -> impl IntoElement {
        let matches = self.matching(cx);
        let entity = cx.entity();
        let empty = matches.is_empty();
        v_flex()
            .id("traffic-table")
            .track_focus(&self.focus)
            .flex_1()
            .min_h(px(105.))
            .overflow_hidden()
            .child(
                columns(cx)
                    .bg(cx.theme().table_head)
                    .text_color(cx.theme().table_head_foreground)
                    .children(Column::ALL.map(|column| {
                        let mut title = column.title().to_owned();
                        if self.sort.column == column {
                            title.push_str(if self.sort.descending { " ↓" } else { " ↑" });
                        }
                        let heading = if column == Column::Path {
                            path_cell(title)
                        } else {
                            cell(title, column.width())
                        };
                        heading
                            .id(column.title())
                            .cursor_pointer()
                            .hover(|el| el.bg(cx.theme().table_hover))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.sort.select(column);
                                this.focus.focus(window, cx);
                                cx.notify();
                            }))
                    })),
            )
            .when(empty, |panel| {
                panel.child(
                    v_flex()
                        .flex_1()
                        .justify_center()
                        .items_center()
                        .gap_2()
                        .child(
                            Icon::new(IconName::Activity)
                                .size_6()
                                .text_color(cx.theme().muted_foreground),
                        )
                        .child(
                            div()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .child("No matching traffic"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("Start capture and route a program through the proxy."),
                        ),
                )
            })
            .when(!empty, |panel| {
                panel.child(
                    uniform_list("traffic-flows", matches.len(), move |range, _, cx| {
                        let this = entity.read(cx);
                        range
                            .filter_map(|i| {
                                let row = this.rows.get(*matches.get(i)?)?;
                                Some(flow_row(row, this.selected, &entity, cx))
                            })
                            .collect::<Vec<_>>()
                    })
                    .track_scroll(&self.list_scroll)
                    .w_full()
                    .flex_1()
                    .min_h_0(),
                )
            })
    }

    fn copy_curl(&self, cx: &mut App) {
        let Some(detail) = &self.detail else { return };
        let raw = if self.include_secrets && !detail.key.reveal {
            self.capture
                .read(cx)
                .session
                .as_ref()
                .and_then(|session| session.flow(detail.key.id, true))
        } else {
            None
        };
        if let Some(command) =
            zentop_proxy::copy_as_curl(raw.as_ref().unwrap_or(&detail.flow), self.include_secrets)
        {
            cx.write_to_clipboard(ClipboardItem::new_string(command));
        }
    }

    fn detail(&self, detail: &Detail, cx: &mut Context<Self>) -> Div {
        let flow = &detail.flow;
        let detail_id = flow.metadata.id;
        let Metadata { kind, state, .. } = &flow.metadata;
        let reason = state.error_reason().map(str::to_owned);
        let owner_note = match flow.metadata.process {
            Attribution::Unknown(reason) => Some(reason),
            _ => None,
        };
        let request_url = flow.request.url.clone();
        let copy_url = request_url.clone();
        v_flex().flex_1().min_h(px(180.)).overflow_hidden().border_t_1().border_color(cx.theme().border)
            .child(v_flex().px_3().pt_2().pb_1().gap_1().bg(theme::palette(cx).card)
                .child(h_flex().gap_2()
                    .child(div().font_weight(gpui_kit::FontWeight::SEMIBOLD).text_sm().child(flow.request.method.clone()))
                    .child(div().min_w_0().flex_1().truncate().text_sm().child(request_url.clone()))
                    .child(Button::new("traffic-copy-url").outline().small().label("Copy URL")
                        .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy_url.clone()))))
                    .child(Checkbox::new("traffic-reveal").checked(self.reveal).label("Reveal secrets")
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.reveal = *checked;
                            this.detail = None;
                            this.preview_task = None;
                            this.refresh_detail(cx);
                            cx.notify();
                        }))))
                .when(matches!(kind, FlowKind::Http | FlowKind::DecryptedHttps), |header| {
                    header.child(h_flex().gap_2()
                        .child(Button::new("traffic-copy-curl").outline().small().label("Copy as cURL")
                            .on_click(cx.listener(|this, _, _, cx| this.copy_curl(cx))))
                        .child(Button::new("traffic-repeat").outline().small().label("Repeat")
                            .disabled(!flow.replayable || self.counters.paused)
                            .on_click(cx.listener(move |this, _, _, cx| this.repeat(detail_id, cx))))
                        .child(Button::new("traffic-edit-replay").outline().small().label("Edit and resend")
                            .disabled(!flow.replayable || self.counters.paused)
                            .on_click(cx.listener(move |this, _, _, cx| this.open_editor(detail_id, cx))))
                        .child(Checkbox::new("traffic-copy-secrets").checked(self.include_secrets)
                            .label("Include secrets in cURL")
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.include_secrets = *checked;
                                cx.notify();
                            }))))
                })
                .child(div().text_xs().text_color(cx.theme().muted_foreground)
                    .child("Bodies and custom headers can contain secrets. Review them before copying."))
                .when_some(reason, |d, reason| d.child(div().text_sm().text_color(theme::palette(cx).danger).child(reason)))
                .child(h_flex().gap_1()
                    .children([Tab::Request, Tab::Response, Tab::Timing, Tab::Messages]
                        .into_iter().filter(|tab| *tab != Tab::Messages || flow.websocket)
                        .map(|tab| {
                        Button::new(tab.name()).ghost().small().selected(self.tab == tab).label(tab.name())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.tab = tab;
                                cx.notify();
                            }))
                    }))
                    .child(div().flex_1())
                    .when_some(owner_note, |row, reason| row.child(div().text_xs()
                        .text_color(cx.theme().muted_foreground).child(reason)))))
            .child(v_flex().id((self.tab.name(), flow.metadata.id as usize)).flex_1().min_h_0()
                .when(self.tab != Tab::Messages, |el| el.overflow_y_scroll())
                .px_3().py_2().gap_2()
                .child(match self.tab {
                    Tab::Request => message(&flow.request.headers, &flow.request_body, detail.previews.as_ref().map(|previews| &previews[0]), flow.headers_truncated, cx).into_any_element(),
                    Tab::Response => flow.response.as_ref().map(|head| {
                        message(&head.headers, &flow.response_body, detail.previews.as_ref().map(|previews| &previews[1]), flow.headers_truncated, cx).into_any_element()
                    }).unwrap_or_else(|| div().text_sm().child("No response headers").into_any_element()),
                    Tab::Timing => timing(flow, cx).into_any_element(),
                    Tab::Messages => self.messages(flow, cx).into_any_element(),
                })
                .when(*kind == FlowKind::Tunnel && self.tab != Tab::Timing, |d| {
                    d.child(div().text_xs().text_color(cx.theme().muted_foreground)
                        .child(flow.metadata.host.rsplit_once(':')
                            .and_then(|(host, _)| zentop_proxy::skip_reason(host))
                            .unwrap_or("CONNECT traffic is not decrypted.")))
                }))
    }
}

impl Render for Traffic {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_kit::component::resizable::{resizable_panel, v_resizable};
        let table = self.table(cx);
        let panes = if let Some(detail) = &self.detail {
            v_resizable("traffic-split")
                .child(
                    resizable_panel()
                        .size(px(226.))
                        .size_range(px(100.)..gpui_kit::Pixels::MAX)
                        .child(table),
                )
                .child(
                    resizable_panel()
                        .size_range(px(180.)..gpui_kit::Pixels::MAX)
                        .child(self.detail(detail, cx)),
                )
                .into_any_element()
        } else {
            table.into_any_element()
        };
        v_flex()
            .id("traffic-window")
            .capture_key_down(cx.listener(Self::key))
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(TitleBar::new().child(div().text_sm().child("Traffic")))
            .child(self.header(cx))
            .child(self.certificate_panel(cx))
            .child(self.toolbar(cx))
            .child(
                h_flex()
                    .items_stretch()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .when(self.show_sidebar, |row| row.child(self.sidebar(cx)))
                    .child(v_flex().flex_1().min_w_0().min_h_0().child(panes)),
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

fn kind_name(kind: Option<FlowKind>) -> &'static str {
    match kind {
        None => "Any kind",
        Some(FlowKind::Http) => "HTTP",
        Some(FlowKind::Tunnel) => "Tunnel",
        Some(FlowKind::Upgrade) => "Upgrade",
        Some(FlowKind::DecryptedHttps) => "Decrypted HTTPS",
    }
}

fn copy_line(id: &'static str, label: &'static str, value: String, cx: &App) -> Div {
    h_flex()
        .min_w_0()
        .gap_2()
        .child(
            div()
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

fn columns(cx: &App) -> Div {
    h_flex()
        .w_full()
        .h(px(33.))
        .gap_2()
        .px_3()
        .text_xs()
        .border_b_1()
        .border_color(cx.theme().table_row_border)
}

fn cell(text: impl Into<SharedString>, width: f32) -> Div {
    div()
        .w(px(width))
        .flex_shrink_0()
        .truncate()
        .child(text.into())
}

fn path_cell(text: impl Into<SharedString>) -> Div {
    div().flex_1().min_w_0().truncate().child(text.into())
}

fn flow_row(
    row: &Metadata,
    selected: Option<u64>,
    entity: &Entity<Traffic>,
    cx: &App,
) -> AnyElement {
    let id = row.id;
    let entity = entity.clone();
    let color = match (row.state.has_error(), row.status) {
        (true, _) | (_, Some(500..=599)) => theme::palette(cx).danger,
        (_, Some(400..=499)) => theme::palette(cx).warning,
        _ => cx.theme().foreground,
    };
    let icon = identity(&row.process)
        .and_then(|owner| entity.read(cx).icons.get(&owner))
        .and_then(Clone::clone);
    let process = h_flex()
        .w(px(Column::Process.width()))
        .flex_shrink_0()
        .gap_1()
        .overflow_hidden()
        .child(if let Some(image) = icon {
            gpui_kit::img(image)
                .size_4()
                .flex_shrink_0()
                .into_any_element()
        } else {
            Icon::new(IconName::AppWindow)
                .size_4()
                .flex_shrink_0()
                .into_any_element()
        })
        .child(if let Some(owner) = identity(&row.process) {
            Button::new(("traffic-process", id as usize))
                .link()
                .small()
                .label(row.process.name().to_owned())
                .on_click(move |_, _, cx| cx.defer(move |cx| super::show_process(owner, cx)))
                .into_any_element()
        } else {
            div()
                .truncate()
                .child(row.process.name().to_owned())
                .into_any_element()
        });
    let open = entity.clone();
    let action = (row.kind == FlowKind::Tunnel).then(|| {
        zentop_proxy::decrypt_host(
            row.host
                .rsplit_once(':')
                .map_or(&row.host, |(host, _)| host),
        )
    });
    let listed = action
        .as_ref()
        .and_then(|result| result.as_ref().ok())
        .is_some_and(|host| certificate::hosts(cx).contains(host));
    columns(cx)
        .id(("traffic-flow", id as usize))
        .cursor_pointer()
        .hover(|d| d.bg(cx.theme().table_hover))
        .when(selected == Some(id), |d| d.bg(cx.theme().table_active))
        .when(row.kind == FlowKind::Tunnel, |d| {
            d.text_color(cx.theme().muted_foreground)
        })
        .on_click(move |_, window, cx| {
            entity.update(cx, |this, cx| {
                this.focus.focus(window, cx);
                if this.selected != Some(id) || !this.detail_open {
                    this.select(Some(id));
                    this.refresh_detail(cx);
                }
                cx.notify();
            })
        })
        .child(cell(
            chrono::DateTime::<chrono::Local>::from(row.started)
                .format("%H:%M:%S")
                .to_string(),
            Column::Time.width(),
        ))
        .child(process)
        .child(cell(row.method.clone(), Column::Method.width()))
        .child(cell(row.host.clone(), Column::Host.width()))
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .overflow_hidden()
                .when(row.repeated, |row| {
                    row.child(
                        div()
                            .text_color(cx.theme().muted_foreground)
                            .child("Repeated"),
                    )
                })
                .child(path_cell(if row.kind == FlowKind::Tunnel {
                    "Not decrypted".into()
                } else if row.kind == FlowKind::DecryptedHttps
                    || (row.kind == FlowKind::Upgrade && row.timing.tls.is_some())
                {
                    format!("Decrypted · {}", row.path)
                } else {
                    row.path.clone()
                }))
                .when_some(action, |row, action| match action {
                    Ok(host) if !listed => row.child(
                        Button::new(("traffic-decrypt-host", id as usize))
                            .outline()
                            .small()
                            .label("Decrypt this host")
                            .on_click(move |_, _, cx| {
                                open.update(cx, |this, cx| {
                                    this.show_certificate = true;
                                    this.edit_host(&host, true, cx);
                                })
                            }),
                    ),
                    Err(reason) => row.child(
                        div()
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(if reason.contains("skip list") {
                                "Skip list"
                            } else {
                                "DNS host required"
                            }),
                    ),
                    _ => row,
                }),
        )
        .child(
            cell(
                row.status
                    .map_or_else(|| "—".into(), |status| status.to_string()),
                Column::Status.width(),
            )
            .text_color(color),
        )
        .child(cell(
            format_bytes(row.bytes.0 + row.bytes.1),
            Column::Size.width(),
        ))
        .child(cell(
            format!("{:.1} ms", row.timing.total.as_secs_f64() * 1_000.),
            Column::Duration.width(),
        ))
        .into_any_element()
}

fn message(
    headers: &Headers,
    body: &Body,
    preview: Option<&Preview>,
    truncated: bool,
    cx: &App,
) -> Div {
    let mono = cx.theme().mono_font_family.clone();
    let headers_view = v_flex()
        .border_1()
        .border_color(cx.theme().border)
        .rounded_md()
        .children(headers.iter().enumerate().map(|(i, (name, value))| {
            h_flex()
                .gap_2()
                .px_2()
                .border_b_1()
                .border_color(cx.theme().table_row_border)
                .child(
                    div()
                        .w(px(165.))
                        .flex_shrink_0()
                        .truncate()
                        .child(name.clone()),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .font_family(mono.clone())
                        .child(value.clone()),
                )
                .child(copy_button(
                    ("traffic-header", i),
                    "header",
                    format!("{name}: {value}"),
                ))
        }));
    let copy = || {
        let body = body.clone();
        Button::new("traffic-copy-body")
            .ghost()
            .small()
            .label("Copy body")
            .on_click(move |_, _, cx| {
                if let Some((_, value)) = zentop_proxy::copy_body(&body) {
                    cx.write_to_clipboard(ClipboardItem::new_string(value));
                }
            })
    };
    let preview = match preview {
        None => div()
            .text_color(cx.theme().muted_foreground)
            .child("Preparing preview…"),
        Some(Preview::Note(note)) => div().text_color(cx.theme().muted_foreground).child(*note),
        Some(Preview::Decoded {
            lines,
            binary,
            limited,
        }) => {
            const ROW_HEIGHT: f32 = 16.;
            let lines = lines.clone();
            let height = px(ROW_HEIGHT * lines.len().min(15) as f32);
            v_flex()
                .gap_1()
                .child(
                    h_flex()
                        .gap_2()
                        .child(if *binary {
                            "Hex preview"
                        } else {
                            "Decoded body"
                        })
                        .child(copy())
                        .when(*limited, |row| {
                            row.child(
                                div()
                                    .text_color(theme::palette(cx).warning)
                                    .child("Preview limited"),
                            )
                        }),
                )
                .child(
                    div().p_2().bg(cx.theme().muted).rounded_md().child(
                        uniform_list("traffic-preview-lines", lines.len(), move |range, _, _| {
                            lines[range]
                                .iter()
                                .map(|line| {
                                    div()
                                        .h(px(ROW_HEIGHT))
                                        .line_height(px(ROW_HEIGHT))
                                        .truncate()
                                        .font_family(mono.clone())
                                        .child(line.clone())
                                })
                                .collect::<Vec<_>>()
                        })
                        .w_full()
                        .h(height),
                    ),
                )
        }
        Some(Preview::Error(error)) => h_flex()
            .gap_2()
            .text_color(theme::palette(cx).warning)
            .child(error.clone())
            .child(copy()),
    };
    v_flex()
        .gap_2()
        .text_xs()
        .child(
            div()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child("Headers"),
        )
        .when(truncated, |d| {
            d.child(
                div()
                    .text_color(theme::palette(cx).warning)
                    .child("Some headers were omitted."),
            )
        })
        .when(headers.is_empty(), |d| d.child("No headers captured"))
        .when(!headers.is_empty(), |d| d.child(headers_view))
        .child(preview)
}

fn timing(flow: &Flow, cx: &App) -> Div {
    let timing = &flow.metadata.timing;
    let duration = |value: Option<Duration>| {
        value.map_or_else(
            || "—".into(),
            |value| format!("{:.1} ms", value.as_secs_f64() * 1_000.),
        )
    };
    v_flex()
        .gap_2()
        .text_sm()
        .child(copy_line(
            "timing-attribution",
            "Attribution wait",
            duration(timing.attribution),
            cx,
        ))
        .child(copy_line(
            "timing-connect",
            "Connect",
            duration(timing.connect),
            cx,
        ))
        .child(copy_line("timing-tls", "TLS", duration(timing.tls), cx))
        .child(copy_line(
            "timing-first-byte",
            "Time to first byte",
            duration(timing.ttfb),
            cx,
        ))
        .child(copy_line(
            "timing-total",
            "Total",
            duration((flow.metadata.state != FlowState::Active).then_some(timing.total)),
            cx,
        ))
        .child(div().child(format!(
            "{} sent · {} received",
            format_bytes(flow.metadata.bytes.0),
            format_bytes(flow.metadata.bytes.1)
        )))
}
