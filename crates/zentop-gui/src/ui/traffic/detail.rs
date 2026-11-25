//! The detail pane: a method/status/URL header, then Request and Response
//! side by side, each with its own tabs. Views build off the UI thread.
use super::body::{lines, note_panel, pairs_list, BodyFormat, BodyView};
use super::parse::{Parsed, SetCookie};
use super::table::{code_color, duration_text, status_label, time_text};
use super::*;
use gpui_kit::component::resizable::{h_resizable, resizable_panel, v_resizable};
use gpui_kit::{FontWeight, Hsla};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum RequestTab {
    #[default]
    Header,
    Query,
    Body,
    Cookies,
    Auth,
    Raw,
    Summary,
}

impl RequestTab {
    const ALL: [Self; 7] = [
        Self::Header,
        Self::Query,
        Self::Body,
        Self::Cookies,
        Self::Auth,
        Self::Raw,
        Self::Summary,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Header => "Header",
            Self::Query => "Query",
            Self::Body => "Body",
            Self::Cookies => "Cookies",
            Self::Auth => "Auth",
            Self::Raw => "Raw",
            Self::Summary => "Summary",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum ResponseTab {
    #[default]
    Header,
    Body,
    SetCookie,
    Raw,
    Messages,
    Timing,
}

impl ResponseTab {
    const ALL: [Self; 6] = [
        Self::Header,
        Self::Body,
        Self::SetCookie,
        Self::Raw,
        Self::Messages,
        Self::Timing,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Header => "Header",
            Self::Body => "Body",
            Self::SetCookie => "Set-Cookie",
            Self::Raw => "Raw",
            Self::Messages => "Messages",
            Self::Timing => "Timing",
        }
    }
}

/// Per-side viewer state, reset when the selection changes.
#[derive(Default)]
pub(super) struct PaneState {
    pub(super) format: BodyFormat,
    pub(super) folded: HashSet<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct DetailKey {
    id: u64,
    revision: u64,
    reveal: bool,
}

pub(super) struct Detail {
    key: DetailKey,
    flow: Arc<Flow>,
    views: Option<Arc<Views>>,
}

type Table = Arc<[(SharedString, SharedString)]>;

/// Everything the panes show, built in the background per flow revision.
pub(super) struct Views {
    bodies: [BodyView; 2],
    raw: [Arc<[SharedString]>; 2],
    query: Table,
    cookies: Parsed<(String, String)>,
    set_cookies: Parsed<SetCookie>,
}

fn table(pairs: impl IntoIterator<Item = (String, String)>) -> Table {
    pairs
        .into_iter()
        .map(|(name, value)| (name.into(), value.into()))
        .collect()
}

impl Views {
    fn build(flow: &Flow) -> Self {
        let no_headers = Vec::new();
        let response_headers = flow
            .response
            .as_ref()
            .map_or(&no_headers, |head| &head.headers);
        let response_body = if flow.response.is_some() || flow.metadata.kind == FlowKind::Upgrade {
            BodyView::build(&flow.response_body, response_headers)
        } else {
            BodyView::build(&Body::Skipped("No response yet"), &no_headers)
        };
        Self {
            bodies: [
                BodyView::build(&flow.request_body, &flow.request.headers),
                response_body,
            ],
            raw: [raw_request(flow), raw_response(flow)],
            query: table(parse::query(&flow.request.url)),
            cookies: parse::cookies(&flow.request.headers),
            set_cookies: parse::set_cookies(response_headers),
        }
    }
}

const RAW_LIMIT: usize = 256 * 1024;

fn raw_body(body: &Body, lines: &mut Vec<SharedString>) {
    match body {
        Body::Skipped(reason) => lines.push(format!("[Body not captured: {reason}]").into()),
        body if body.kept().is_empty() => {}
        body => {
            let kept = body.kept();
            let shown = &kept[..kept.len().min(RAW_LIMIT)];
            match std::str::from_utf8(shown) {
                Ok(text) if !text.contains('\0') => {
                    lines.extend(text.lines().map(|line| line.to_owned().into()))
                }
                _ => lines
                    .push(format!("[{} of binary data]", format_bytes(kept.len() as u64)).into()),
            }
            if kept.len() > RAW_LIMIT {
                lines.push(
                    format!(
                        "[… {} more retained]",
                        format_bytes((kept.len() - RAW_LIMIT) as u64)
                    )
                    .into(),
                );
            }
            if let Body::Truncated { kept, total } = body {
                lines.push(
                    format!(
                        "[{} not retained]",
                        format_bytes(total.saturating_sub(kept.len() as u64))
                    )
                    .into(),
                );
            }
        }
    }
}

fn raw_headers(headers: &Headers, lines: &mut Vec<SharedString>) {
    lines.extend(
        headers
            .iter()
            .map(|(name, value)| format!("{name}: {value}").into()),
    );
}

/// The request as HTTP/1.1-style text, from the redacted projection.
fn raw_request(flow: &Flow) -> Arc<[SharedString]> {
    let request = &flow.request;
    let target = if request.method == "CONNECT" {
        flow.metadata.host.clone()
    } else {
        let url = request.url.as_str();
        url.split_once("://")
            .map_or(url, |(_, rest)| {
                rest.find('/').map_or("/", |start| &rest[start..])
            })
            .to_owned()
    };
    let mut lines = vec![format!("{} {target} {}", request.method, request.version).into()];
    raw_headers(&request.headers, &mut lines);
    lines.push(SharedString::default());
    raw_body(&flow.request_body, &mut lines);
    lines.into()
}

fn raw_response(flow: &Flow) -> Arc<[SharedString]> {
    let Some(response) = &flow.response else {
        return Arc::new(["[No response]".into()]);
    };
    let mut lines = vec![format!(
        "{} {} {}",
        response.version,
        response.status,
        reason(response.status)
    )
    .trim_end()
    .to_owned()
    .into()];
    raw_headers(&response.headers, &mut lines);
    lines.push(SharedString::default());
    raw_body(&flow.response_body, &mut lines);
    lines.into()
}

fn reason(status: u16) -> &'static str {
    match status {
        100 => "Continue",
        101 => "Switching Protocols",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        206 => "Partial Content",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        407 => "Proxy Authentication Required",
        409 => "Conflict",
        410 => "Gone",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        421 => "Misdirected Request",
        422 => "Unprocessable Content",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "",
    }
}

fn status_color(status: Option<u16>, cx: &App) -> Hsla {
    match status {
        Some(200..=299) => theme::palette(cx).running,
        Some(300..=399) => cx.theme().primary,
        Some(_) => code_color(status, cx),
        None => cx.theme().muted_foreground,
    }
}

fn badge(text: impl Into<SharedString>, color: Hsla) -> Div {
    div()
        .flex_shrink_0()
        .px_1p5()
        .rounded_sm()
        .bg(color.opacity(0.15))
        .text_color(color)
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .child(text.into())
}

fn tab_button(id: (&'static str, usize), name: &'static str, selected: bool) -> Button {
    Button::new(id)
        .ghost()
        .xsmall()
        .selected(selected)
        .label(name)
}

impl Traffic {
    pub(super) fn refresh_detail(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.selected else {
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
        // Keep showing the previous views of this flow until new ones are ready.
        let views = self
            .detail
            .as_ref()
            .filter(|detail| detail.key.id == id && detail.key.reveal == reveal)
            .and_then(|detail| detail.views.clone());
        self.detail = Some(Detail {
            key,
            flow: flow.clone(),
            views,
        });
        self.preview_task = Some(cx.spawn(async move |this, cx| {
            let views = cx
                .background_executor()
                .spawn(async move { Arc::new(Views::build(&flow)) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(detail) = this.detail.as_mut().filter(|detail| detail.key == key) {
                    detail.views = Some(views);
                    cx.notify();
                }
            });
        }));
    }

    pub(super) fn set_reveal(&mut self, reveal: bool, cx: &mut Context<Self>) {
        self.reveal = reveal;
        self.detail = None;
        self.preview_task = None;
        self.refresh_detail(cx);
        cx.notify();
    }

    pub(super) fn detail(&self, detail: &Detail, cx: &mut Context<Self>) -> impl IntoElement {
        let request = self.request_pane(detail, cx);
        let response = self.response_pane(detail, cx);
        let panes = if self.placement == Placement::Right {
            v_resizable("traffic-panes-stacked")
        } else {
            h_resizable("traffic-panes")
        };
        v_flex()
            .size_full()
            .min_h_0()
            .overflow_hidden()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(self.detail_header(detail, cx))
            .child(
                div().flex_1().min_h_0().child(
                    panes
                        .child(
                            resizable_panel()
                                .size_range(px(220.)..gpui_kit::Pixels::MAX)
                                .child(request),
                        )
                        .child(
                            resizable_panel()
                                .size_range(px(220.)..gpui_kit::Pixels::MAX)
                                .child(response),
                        ),
                ),
            )
    }

    fn detail_header(&self, detail: &Detail, cx: &mut Context<Self>) -> impl IntoElement {
        let flow = &detail.flow;
        let id = flow.metadata.id;
        let metadata = &flow.metadata;
        let http = matches!(metadata.kind, FlowKind::Http | FlowKind::DecryptedHttps);
        let replay = flow.replayable && !self.counters.paused;
        let url = flow.request.url.clone();
        let status = metadata.status.map_or_else(
            || status_label(metadata).to_owned(),
            |code| format!("{code} {}", reason(code)).trim_end().to_owned(),
        );
        let decrypt = (metadata.kind == FlowKind::Tunnel)
            .then(|| zentop_proxy::decrypt_host(navigation::hostname(&metadata.host)))
            .map(|host| host.map(|host| (!certificate::hosts(cx).contains(&host)).then_some(host)));
        let note = match (&decrypt, metadata.state.error_reason()) {
            (_, Some(reason)) => Some((reason.to_owned(), theme::palette(cx).danger)),
            (Some(_), None) => Some((
                zentop_proxy::skip_reason(navigation::hostname(&metadata.host))
                    .unwrap_or("CONNECT traffic is not decrypted.")
                    .to_owned(),
                cx.theme().muted_foreground,
            )),
            _ => None,
        };
        v_flex()
            .flex_shrink_0()
            .px_3()
            .py_1p5()
            .gap_1()
            .bg(theme::palette(cx).card)
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .items_start()
                    .gap_2()
                    .child(badge(flow.request.method.clone(), cx.theme().primary))
                    .child(badge(status, status_color(metadata.status, cx)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .font_family(cx.theme().mono_font_family.clone())
                            .child(url.clone()),
                    ),
            )
            .child(
                h_flex()
                    .gap_1()
                    .flex_wrap()
                    .child(
                        Button::new("traffic-copy-url")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Copy)
                            .label("URL")
                            .tooltip("Copy URL")
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(url.clone()))
                            }),
                    )
                    .when(http, |row| {
                        row.child(
                            Button::new("traffic-copy-curl")
                                .ghost()
                                .xsmall()
                                .icon(IconName::SquareTerminal)
                                .label("cURL")
                                .tooltip("Copy as cURL (⇧⌘C)")
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.copy_curl(id, cx)),
                                ),
                        )
                        .child(
                            Button::new("traffic-repeat")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Repeat)
                                .label("Repeat")
                                .tooltip("Repeat (⌘R)")
                                .disabled(!replay)
                                .on_click(cx.listener(move |this, _, _, cx| this.repeat(id, cx))),
                        )
                        .child(
                            Button::new("traffic-edit-replay")
                                .ghost()
                                .xsmall()
                                .icon(IconName::SquarePen)
                                .label("Edit and resend")
                                .tooltip("Edit and resend (⌘E)")
                                .disabled(!replay)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_editor(Some(id), cx)
                                })),
                        )
                    })
                    .when_some(decrypt.and_then(Result::ok).flatten(), |row, host| {
                        row.child(
                            Button::new("traffic-decrypt-host")
                                .ghost()
                                .xsmall()
                                .icon(IconName::LockOpen)
                                .label("Decrypt this host")
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.decrypt_host(&host, cx)),
                                ),
                        )
                    })
                    .child(div().w(px(6.)))
                    .child(
                        Checkbox::new("traffic-reveal")
                            .small()
                            .checked(self.reveal)
                            .label("Reveal secrets")
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.set_reveal(*checked, cx)
                            })),
                    )
                    .when(http, |row| {
                        row.child(
                            Checkbox::new("traffic-copy-secrets")
                                .small()
                                .checked(self.include_secrets)
                                .label("Include secrets in cURL")
                                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                    this.include_secrets = *checked;
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .when_some(note, |row, (note, color)| {
                        row.child(div().min_w_0().truncate().text_color(color).child(note))
                    })
                    .child(div().flex_1())
                    .child("Bodies and custom headers can contain secrets."),
            )
    }

    fn pane(
        &self,
        title: &'static str,
        tabs: impl IntoIterator<Item = Button>,
        content: AnyElement,
        cx: &App,
    ) -> Div {
        v_flex()
            .size_full()
            .min_h_0()
            .overflow_hidden()
            .child(
                h_flex()
                    .flex_shrink_0()
                    .h(px(28.))
                    .px_2()
                    .gap_0p5()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .pr_2()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .children(tabs),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .p_2()
                    .text_xs()
                    .child(content),
            )
    }

    fn request_pane(&self, detail: &Detail, cx: &mut Context<Self>) -> Div {
        let flow = &detail.flow;
        let views = detail.views.as_deref();
        let tabs = RequestTab::ALL.map(|tab| {
            tab_button(
                ("traffic-request-tab", tab as usize),
                tab.name(),
                self.request_tab == tab,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.request_tab = tab;
                cx.notify();
            }))
        });
        let preparing = || note_panel("Preparing…", cx).into_any_element();
        let content = match self.request_tab {
            RequestTab::Header => headers_table(
                "traffic-request-headers",
                &flow.request.headers,
                flow.headers_truncated,
                cx,
            )
            .into_any_element(),
            RequestTab::Query => match views {
                None => preparing(),
                Some(views) if views.query.is_empty() => {
                    note_panel("No query parameters", cx).into_any_element()
                }
                Some(views) => {
                    pairs_list("traffic-query", views.query.clone(), cx).into_any_element()
                }
            },
            RequestTab::Body => self
                .body_viewer(
                    0,
                    views.map(|views| &views.bodies[0]),
                    &flow.request_body,
                    cx,
                )
                .into_any_element(),
            RequestTab::Cookies => match views.map(|views| &views.cookies) {
                None => preparing(),
                Some(Parsed::Masked) => masked_note(cx).into_any_element(),
                Some(Parsed::Values(values)) if values.is_empty() => {
                    note_panel("No cookies", cx).into_any_element()
                }
                Some(Parsed::Values(values)) => {
                    pairs_list("traffic-cookies", table(values.iter().cloned()), cx)
                        .into_any_element()
                }
            },
            RequestTab::Auth => auth(flow, cx).into_any_element(),
            RequestTab::Raw => match views {
                None => preparing(),
                Some(views) => {
                    lines("traffic-request-raw", views.raw[0].clone(), true, cx).into_any_element()
                }
            },
            RequestTab::Summary => summary(flow, cx).into_any_element(),
        };
        self.pane("Request", tabs, content, cx)
    }

    fn response_pane(&self, detail: &Detail, cx: &mut Context<Self>) -> Div {
        let flow = &detail.flow;
        let views = detail.views.as_deref();
        let tab = if self.response_tab == ResponseTab::Messages && !flow.websocket {
            ResponseTab::Header
        } else {
            self.response_tab
        };
        let tabs = ResponseTab::ALL
            .into_iter()
            .filter(|tab| *tab != ResponseTab::Messages || flow.websocket)
            .map(|option| {
                tab_button(
                    ("traffic-response-tab", option as usize),
                    option.name(),
                    tab == option,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.response_tab = option;
                    cx.notify();
                }))
            })
            .collect::<Vec<_>>();
        let preparing = || note_panel("Preparing…", cx).into_any_element();
        let content = match tab {
            ResponseTab::Header => match &flow.response {
                Some(head) => headers_table(
                    "traffic-response-headers",
                    &head.headers,
                    flow.headers_truncated,
                    cx,
                )
                .into_any_element(),
                None => note_panel("No response yet", cx).into_any_element(),
            },
            ResponseTab::Body => self
                .body_viewer(
                    1,
                    views.map(|views| &views.bodies[1]),
                    &flow.response_body,
                    cx,
                )
                .into_any_element(),
            ResponseTab::SetCookie => match views.map(|views| &views.set_cookies) {
                None => preparing(),
                Some(Parsed::Masked) => masked_note(cx).into_any_element(),
                Some(Parsed::Values(cookies)) if cookies.is_empty() => {
                    note_panel("No cookies set", cx).into_any_element()
                }
                Some(Parsed::Values(cookies)) => set_cookie_list(cookies, cx).into_any_element(),
            },
            ResponseTab::Raw => match views {
                None => preparing(),
                Some(views) => {
                    lines("traffic-response-raw", views.raw[1].clone(), true, cx).into_any_element()
                }
            },
            ResponseTab::Messages => self.messages(flow, cx).into_any_element(),
            ResponseTab::Timing => timing(flow, cx).into_any_element(),
        };
        self.pane("Response", tabs, content, cx)
    }
}

fn masked_note(cx: &App) -> Div {
    note_panel(
        "Cookie values are masked. Turn on Reveal secrets to show them for this flow.",
        cx,
    )
}

fn headers_table(
    id: &'static str,
    headers: &Headers,
    truncated: bool,
    cx: &App,
) -> impl IntoElement {
    let mono = cx.theme().mono_font_family.clone();
    v_flex()
        .id(id)
        .size_full()
        .overflow_y_scroll()
        .when(truncated, |table| {
            table.child(
                div()
                    .pb_1()
                    .text_color(theme::palette(cx).warning)
                    .child("Some headers were omitted."),
            )
        })
        .when(headers.is_empty(), |table| {
            table.child(note_panel("No headers captured", cx))
        })
        .children(headers.iter().enumerate().map(|(index, (name, value))| {
            h_flex()
                .min_h(px(22.))
                .gap_2()
                .px_1()
                .border_b_1()
                .border_color(cx.theme().table_row_border)
                .child(
                    div()
                        .w(px(150.))
                        .flex_shrink_0()
                        .truncate()
                        .text_color(cx.theme().muted_foreground)
                        .child(name.clone()),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .font_family(mono.clone())
                        .child(value.clone()),
                )
                .child(copy_button(
                    (id, index),
                    "header",
                    format!("{name}: {value}"),
                ))
        }))
}

fn rows(id: &'static str, rows: Vec<(&'static str, String)>, cx: &App) -> impl IntoElement {
    v_flex()
        .id(id)
        .size_full()
        .overflow_y_scroll()
        .children(rows.into_iter().map(|(name, value)| {
            h_flex()
                .min_h(px(22.))
                .gap_2()
                .px_1()
                .border_b_1()
                .border_color(cx.theme().table_row_border)
                .child(
                    div()
                        .w(px(130.))
                        .flex_shrink_0()
                        .text_color(cx.theme().muted_foreground)
                        .child(name),
                )
                .child(div().min_w_0().flex_1().child(value))
        }))
}

fn auth(flow: &Flow, cx: &App) -> impl IntoElement {
    let value = flow
        .request
        .headers
        .iter()
        .find(|(name, _)| name == "authorization")
        .map(|(_, value)| value.clone());
    let Some(value) = value else {
        return v_flex()
            .child(note_panel("No Authorization header", cx))
            .child(note_panel(
                "Proxy credentials are consumed by Zentop and never recorded.",
                cx,
            ))
            .into_any_element();
    };
    let scheme = flow.auth_scheme.clone().unwrap_or_else(|| "—".into());
    // Without Reveal secrets, the value is masked by the shared policy.
    let credentials = match &flow.auth_scheme {
        Some(scheme) if value != zentop_proxy::MASK => value
            .strip_prefix(scheme.as_str())
            .map_or(value.clone(), |rest| rest.trim().to_owned()),
        _ => value,
    };
    rows(
        "traffic-auth",
        vec![("Scheme", scheme), ("Credentials", credentials)],
        cx,
    )
    .into_any_element()
}

fn summary(flow: &Flow, cx: &mut Context<Traffic>) -> impl IntoElement {
    let metadata = &flow.metadata;
    let timing = &metadata.timing;
    let ms = |value: Option<Duration>| value.map_or_else(|| "—".into(), duration_text);
    let process = identity(&metadata.process);
    let attribution = match &metadata.process {
        Attribution::Known { pid, name, .. } => format!("{name} (PID {pid})"),
        Attribution::Unknown(reason) => format!("Unknown: {reason}"),
    };
    let kind = match metadata.kind {
        FlowKind::Http => "HTTP",
        FlowKind::DecryptedHttps => "Decrypted HTTPS",
        FlowKind::Tunnel => "CONNECT tunnel (not decrypted)",
        FlowKind::Upgrade if flow.websocket => "WebSocket upgrade",
        FlowKind::Upgrade => "Upgraded connection",
    };
    v_flex()
        .size_full()
        .gap_1()
        .when_some(process, |panel, owner| {
            panel.child(
                h_flex().gap_2().child(
                    Button::new("traffic-process-link")
                        .link()
                        .xsmall()
                        .label(format!("Open {} details", metadata.process.name()))
                        .on_click(move |_, _, cx| {
                            cx.defer(move |cx| super::super::show_process(owner, cx))
                        }),
                ),
            )
        })
        .child(rows(
            "traffic-summary",
            vec![
                ("Flow", format!("#{}", metadata.id)),
                ("Process", attribution),
                ("Connection", format!("#{}", metadata.connection)),
                ("Kind", kind.into()),
                ("Content", metadata.content.name().into()),
                ("Status", status_label(metadata).into()),
                ("Started", time_text(metadata.started)),
                ("Attribution wait", ms(timing.attribution)),
                (
                    "Duration",
                    ms((metadata.state != FlowState::Active).then_some(timing.total)),
                ),
                ("Request size", format_bytes(metadata.bytes.0)),
                ("Response size", format_bytes(metadata.bytes.1)),
                (
                    "Repeated",
                    if metadata.repeated { "Yes" } else { "No" }.into(),
                ),
            ],
            cx,
        ))
}

fn set_cookie_list(cookies: &[SetCookie], cx: &App) -> impl IntoElement {
    let mono = cx.theme().mono_font_family.clone();
    v_flex()
        .id("traffic-set-cookies")
        .size_full()
        .overflow_y_scroll()
        .gap_1()
        .children(cookies.iter().map(|cookie| {
            v_flex()
                .p_1()
                .gap_0p5()
                .border_b_1()
                .border_color(cx.theme().table_row_border)
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(cookie.name.clone()),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .truncate()
                                .font_family(mono.clone())
                                .child(cookie.value.clone()),
                        ),
                )
                .child(
                    h_flex()
                        .gap_1()
                        .flex_wrap()
                        .children(cookie.attributes.iter().map(|(name, value)| {
                            badge(
                                match value {
                                    Some(value) => format!("{name}={value}"),
                                    None => name.clone(),
                                },
                                cx.theme().muted_foreground,
                            )
                            .font_weight(FontWeight::NORMAL)
                        })),
                )
        }))
}

fn timing(flow: &Flow, cx: &App) -> impl IntoElement {
    let timing = &flow.metadata.timing;
    let ms = |value: Option<Duration>| {
        value.map_or_else(
            || "—".into(),
            |value| format!("{:.1} ms", value.as_secs_f64() * 1_000.),
        )
    };
    rows(
        "traffic-timing",
        vec![
            ("Attribution wait", ms(timing.attribution)),
            ("Connect", ms(timing.connect)),
            ("TLS", ms(timing.tls)),
            ("Time to first byte", ms(timing.ttfb)),
            (
                "Total",
                ms((flow.metadata.state != FlowState::Active).then_some(timing.total)),
            ),
            ("Sent", format_bytes(flow.metadata.bytes.0)),
            ("Received", format_bytes(flow.metadata.bytes.1)),
        ],
        cx,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use zentop_proxy::{RequestHead, ResponseHead};

    #[test]
    fn raw_text_uses_origin_form_and_the_redacted_projection() {
        let flow = Flow {
            metadata: navigation::tests::row(1, "x.test:80"),
            request: RequestHead {
                method: "POST".into(),
                url: "http://x.test/a/b?token=[redacted]".into(),
                host: "x.test:80".into(),
                path: "/a/b".into(),
                version: "HTTP/1.1".into(),
                headers: vec![("cookie".into(), zentop_proxy::MASK.into())],
            },
            response: Some(ResponseHead {
                status: 404,
                version: "HTTP/1.1".into(),
                headers: Vec::new(),
            }),
            request_body: Body::Captured(Arc::new(b"a=1\nb=2".to_vec())),
            response_body: Body::Truncated {
                kept: Arc::new(vec![0, 1, 2]),
                total: 10,
            },
            headers_truncated: false,
            replayable: true,
            messages: Vec::new(),
            message_count: 0,
            websocket: false,
            auth_scheme: None,
        };
        let request: Vec<_> = raw_request(&flow).iter().map(ToString::to_string).collect();
        assert_eq!(
            request,
            [
                "POST /a/b?token=[redacted] HTTP/1.1",
                "cookie: [redacted]",
                "",
                "a=1",
                "b=2"
            ]
        );
        let response: Vec<_> = raw_response(&flow)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            response,
            [
                "HTTP/1.1 404 Not Found",
                "",
                "[3.0 B of binary data]",
                "[7.0 B not retained]"
            ]
        );
        let views = Views::build(&flow);
        assert_eq!(views.query.len(), 1);
        assert_eq!(views.cookies, Parsed::Masked);
    }
}
