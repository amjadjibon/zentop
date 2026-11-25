use std::path::PathBuf;

use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::{FontWeight, ScrollStrategy, WeakEntity};
use zentop_proxy::{
    Direction, HarOptions, Message, Opcode, ReplayDraft, ReplayEdits, ReplayHeader, ReplayValue,
};

use super::*;

impl Traffic {
    pub(super) fn sidebar(&self, cx: &Context<Self>) -> impl IntoElement {
        let label = |title| {
            div()
                .px_2()
                .pt_2()
                .pb_1()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title)
        };
        let mut index = 0_usize;
        let mut item = |scope: Scope, name: &str, count: usize, app, nested| {
            index += 1;
            let icon = if let Scope::App(key) = &scope {
                self.sidebar
                    .apps
                    .get(key)
                    .and_then(|group| group.identity)
                    .and_then(|id| self.icons.get(&id))
                    .and_then(Clone::clone)
            } else {
                None
            };
            let selected = self.scope.as_ref() == Some(&scope);
            h_flex()
                .id(("traffic-scope", index))
                .min_w_0()
                .gap_1()
                .px_2()
                .h(px(27.))
                .rounded_md()
                .cursor_pointer()
                .when(nested, |row| row.pl_5())
                .hover(|row| row.bg(cx.theme().table_hover))
                .when(selected, |row| row.bg(cx.theme().table_active))
                .when(app, |row| {
                    row.child(if let Some(icon) = icon {
                        gpui_kit::img(icon)
                            .size_4()
                            .flex_shrink_0()
                            .into_any_element()
                    } else {
                        Icon::new(IconName::AppWindow)
                            .size_4()
                            .flex_shrink_0()
                            .into_any_element()
                    })
                })
                .child(div().flex_1().min_w_0().truncate().child(name.to_owned()))
                .child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child(count.to_string()),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.scope = if this.scope.as_ref() == Some(&scope) {
                        None
                    } else {
                        Some(scope.clone())
                    };
                    this.focus.focus(window, cx);
                    this.list_scroll.scroll_to_item(0, ScrollStrategy::Top);
                    cx.notify();
                }))
        };
        let apps: Vec<_> = self
            .sidebar
            .apps
            .iter()
            .map(|(app, group)| item(Scope::App(app.clone()), &app.name, group.count, true, false))
            .collect();
        let mut domains = Vec::new();
        for (name, group) in &self.sidebar.domains {
            let grouped = group.hosts.len() > 1;
            if grouped {
                domains.push(item(
                    Scope::Domain(name.clone()),
                    name,
                    group.count,
                    false,
                    false,
                ));
            }
            domains.extend(group.hosts.iter().map(|(host, &count)| {
                item(Scope::Host(host.clone()), host, count, false, grouped)
            }));
        }
        v_flex()
            .id("traffic-sidebar")
            .w(px(175.))
            .h_full()
            .min_h_0()
            .flex_shrink_0()
            .overflow_y_scroll()
            .p_1()
            .text_xs()
            .bg(theme::palette(cx).card)
            .border_r_1()
            .border_color(cx.theme().border)
            .child(label("Apps"))
            .children(apps)
            .child(label("Domains"))
            .children(domains)
    }

    pub(super) fn key(
        &mut self,
        event: &gpui_kit::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = &event.keystroke;
        if key.key == "escape" || (key.modifiers.platform && key.key == "k") {
            cx.stop_propagation();
            for input in &self.filters {
                input.update(cx, |input, cx| input.set_value("", window, cx));
            }
            self.scope = None;
            self.process_scope = None;
            self.status = None;
            self.kind = None;
            self.errors = false;
            cx.notify();
        } else if self.focus.is_focused(window) && !key.modifiers.modified() {
            match key.key.as_str() {
                "up" | "down" => {
                    let rows = self.matching(cx);
                    let Some(last) = rows.len().checked_sub(1) else {
                        return;
                    };
                    let index = rows
                        .iter()
                        .position(|&index| Some(self.rows[index].id) == self.selected)
                        .map_or(if key.key == "up" { last } else { 0 }, |index| {
                            if key.key == "up" {
                                index.saturating_sub(1)
                            } else {
                                (index + 1).min(last)
                            }
                        });
                    let detail_open = self.detail_open;
                    self.select(Some(self.rows[rows[index]].id));
                    self.detail_open = detail_open;
                    self.refresh_detail(cx);
                    self.list_scroll
                        .scroll_to_item(index, ScrollStrategy::Nearest);
                    cx.stop_propagation();
                    cx.notify();
                }
                "enter" if self.selected.is_some() => {
                    self.detail_open = true;
                    self.tab = Tab::Request;
                    self.refresh_detail(cx);
                    cx.stop_propagation();
                    cx.notify();
                }
                _ => {}
            }
        }
    }

    pub(super) fn export_har(&mut self, selected_only: bool, cx: &mut Context<Self>) {
        let Some(session) = self.capture.read(cx).session.as_ref() else {
            return;
        };
        let ids = if selected_only {
            self.selected.into_iter().collect()
        } else {
            self.matching(cx)
                .iter()
                .map(|&index| self.rows[index].id)
                .collect()
        };
        let export = session.har_export(ids);
        let options = HarOptions {
            include_bodies: self.include_bodies,
        };
        let directory = dirs::download_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));
        let path = cx.prompt_for_new_path(&directory, Some("zentop-traffic.har"));
        cx.spawn(async move |this, cx| {
            let result = match path.await {
                Ok(Ok(Some(mut path))) => {
                    if path.extension().is_none() {
                        path.set_extension("har");
                    }
                    cx.background_executor()
                        .spawn(async move {
                            let file = std::fs::File::create(&path)?;
                            let count = export.write(std::io::BufWriter::new(file), options)?;
                            Ok::<_, std::io::Error>(format!(
                                "Exported {count} flows to {}",
                                path.display()
                            ))
                        })
                        .await
                        .map_err(|error| format!("HAR export failed: {error}"))
                }
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(error)) => Err(format!("HAR export failed: {error}")),
            };
            let _ = this.update(cx, |this, cx| {
                this.operation = Some(result);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn repeat(&mut self, id: u64, cx: &mut Context<Self>) {
        let result = self
            .capture
            .read(cx)
            .session
            .as_ref()
            .ok_or("Capture stopped")
            .and_then(|session| session.replay(id, ReplayEdits::default()));
        match result {
            Ok(result) => {
                let generation = self.generation;
                self.operation = Some(Ok("Repeating request…".into()));
                cx.spawn(async move |this, cx| {
                    let result = result
                        .await
                        .unwrap_or_else(|_| Err("Capture stopped".into()));
                    let _ = this.update(cx, |this, cx| this.replay_result(generation, result, cx));
                })
                .detach();
            }
            Err(error) => self.operation = Some(Err(error.into())),
        }
        cx.notify();
    }

    fn replay_result(
        &mut self,
        generation: u64,
        result: Result<u64, String>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            return;
        }
        self.operation = Some(match result {
            Ok(id) => {
                self.refresh(cx);
                if let Some(index) = self
                    .matching(cx)
                    .iter()
                    .position(|&index| self.rows[index].id == id)
                {
                    self.list_scroll
                        .scroll_to_item(index, ScrollStrategy::Nearest);
                }
                self.select(Some(id));
                self.refresh_detail(cx);
                Ok("Repeated request captured".into())
            }
            Err(error) => Err(error),
        });
        cx.notify();
    }

    pub(super) fn open_editor(&mut self, flow: u64, cx: &mut Context<Self>) {
        let Some(draft) = self
            .capture
            .read(cx)
            .session
            .as_ref()
            .and_then(|session| session.replay_draft(flow, self.reveal))
        else {
            self.operation = Some(Err("The original request is no longer available".into()));
            cx.notify();
            return;
        };
        let options = super::super::window_options(
            size(px(760.), px(630.)),
            Some(size(px(525.), px(455.))),
            cx,
        );
        let source = cx.entity().downgrade();
        let generation = self.generation;
        let masked = !self.reveal;
        if let Err(error) = gpui_kit::open_window(options, cx, move |window, cx| {
            window.set_window_title("Edit and resend");
            cx.new(|cx| Editor::new(source, generation, flow, masked, draft, window, cx))
        }) {
            self.operation = Some(Err(format!("Could not open request editor: {error}")));
            cx.notify();
        }
    }

    pub(super) fn messages(&self, flow: &Arc<Flow>, cx: &Context<Self>) -> Div {
        let entity = cx.entity();
        let messages = flow.clone();
        let selected = self.message_row;
        v_flex()
            .size_full()
            .min_h_0()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{} of {} frames retained · Payloads may contain secrets",
                        flow.messages.len(),
                        flow.message_count
                    )),
            )
            .child(
                uniform_list(
                    "traffic-websocket-frames",
                    flow.messages.len(),
                    move |range, _, cx| {
                        range
                            .filter_map(|index| {
                                let message = messages.messages.get(index)?;
                                let entity = entity.clone();
                                Some(
                                    h_flex()
                                        .id(("traffic-websocket-frame", index))
                                        .w_full()
                                        .h(px(29.))
                                        .gap_2()
                                        .text_xs()
                                        .cursor_pointer()
                                        .overflow_hidden()
                                        .hover(|row| row.bg(cx.theme().table_hover))
                                        .when(selected == Some(index), |row| {
                                            row.bg(cx.theme().table_active)
                                        })
                                        .on_click(move |_, _, cx| {
                                            entity.update(cx, |this, cx| {
                                                this.message_row = (this.message_row
                                                    != Some(index))
                                                .then_some(index);
                                                cx.notify();
                                            })
                                        })
                                        .child(cell(
                                            chrono::DateTime::<chrono::Local>::from(message.time)
                                                .format("%H:%M:%S%.3f")
                                                .to_string(),
                                            91.,
                                        ))
                                        .child(cell(
                                            match message.direction {
                                                Direction::ClientToServer => "Sent",
                                                Direction::ServerToClient => "Received",
                                            },
                                            63.,
                                        ))
                                        .child(cell(opcode(message.opcode), 90.))
                                        .child(cell(format_bytes(message.size), 70.))
                                        .child(path_cell(message_text(message, 128))),
                                )
                            })
                            .collect::<Vec<_>>()
                    },
                )
                .flex_1()
                .min_h(px(30.)),
            )
            .when_some(
                self.message_row.and_then(|index| flow.messages.get(index)),
                |panel, message| {
                    panel.child(
                        div()
                            .id("traffic-frame-payload")
                            .max_h(px(110.))
                            .overflow_y_scroll()
                            .p_2()
                            .text_xs()
                            .bg(theme::palette(cx).card)
                            .font_family(cx.theme().mono_font_family.clone())
                            .child(message_text(message, 16 * 1024)),
                    )
                },
            )
    }
}

fn opcode(opcode: Opcode) -> String {
    match opcode {
        Opcode::Text => "Text".into(),
        Opcode::Binary => "Binary".into(),
        Opcode::Continuation => "Continuation".into(),
        Opcode::Ping => "Ping".into(),
        Opcode::Pong => "Pong".into(),
        Opcode::Close(Some(code)) => format!("Close {code}"),
        Opcode::Close(None) => "Close".into(),
        Opcode::Other(code) => format!("Opcode {code}"),
    }
}

fn message_text(message: &Message, limit: usize) -> String {
    if let Some(note) = message.note {
        return note.into();
    }
    let bytes = &message.payload[..message.payload.len().min(limit)];
    let mut text = if message.text {
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        use std::fmt::Write as _;
        let mut hex = String::with_capacity(bytes.len() * 3);
        for (index, byte) in bytes.iter().enumerate() {
            if index != 0 {
                hex.push(if index % 32 == 0 { '\n' } else { ' ' });
            }
            let _ = write!(hex, "{byte:02x}");
        }
        hex
    };
    if message.payload.len() > limit {
        text.push('…');
    }
    text
}

struct HeaderEditor {
    key: usize,
    original: Option<usize>,
    name: Entity<InputState>,
    value: Entity<InputState>,
    dirty: bool,
    _change: Subscription,
}

struct Editor {
    source: WeakEntity<Traffic>,
    generation: u64,
    id: u64,
    masked: bool,
    draft: ReplayDraft,
    method: Entity<InputState>,
    url: Entity<InputState>,
    body: Entity<TextareaState>,
    headers: Vec<HeaderEditor>,
    next_key: usize,
    waiting: bool,
    result: Option<Result<(), String>>,
}

impl Editor {
    fn new(
        source: WeakEntity<Traffic>,
        generation: u64,
        id: u64,
        masked: bool,
        draft: ReplayDraft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let method = cx.new(|cx| InputState::new(window, cx).default_value(draft.method.clone()));
        let url = cx.new(|cx| InputState::new(window, cx).default_value(draft.url.clone()));
        let body = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(5)
                .placeholder(if draft.body.is_none() {
                    "Binary body is reused unless you enter text"
                } else {
                    "Request body"
                })
                .default_value(draft.body.clone().unwrap_or_default())
        });
        let headers = draft
            .headers
            .iter()
            .enumerate()
            .map(|(index, (name, value))| Self::header(index, Some(index), name, value, window, cx))
            .collect();
        Self {
            source,
            generation,
            id,
            masked,
            next_key: draft.headers.len(),
            draft,
            method,
            url,
            body,
            headers,
            waiting: false,
            result: None,
        }
    }

    fn header(
        key: usize,
        original: Option<usize>,
        name: &str,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> HeaderEditor {
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Header name")
                .default_value(name.to_owned())
        });
        let value = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Header value")
                .default_value(value.to_owned())
        });
        let change = cx.subscribe_in(&value, window, move |this, _, event: &InputEvent, _, _| {
            if let InputEvent::Change = event {
                if let Some(header) = this.headers.iter_mut().find(|row| row.key == key) {
                    header.dirty = true;
                }
            }
        });
        HeaderEditor {
            key,
            original,
            name,
            value,
            dirty: false,
            _change: change,
        }
    }

    fn send(&mut self, cx: &mut Context<Self>) {
        let method = self.method.read(cx).value();
        let url = self.url.read(cx).value();
        let body = self.body.read(cx).value();
        let edits = ReplayEdits {
            method: (method.as_ref() != self.draft.method).then(|| method.to_string()),
            url: (url.as_ref() != self.draft.url).then(|| url.to_string()),
            body: (body.as_ref() != self.draft.body.as_deref().unwrap_or_default())
                .then(|| body.to_string()),
            preserve_masked_query: self.masked,
            headers: Some(
                self.headers
                    .iter()
                    .map(|row| ReplayHeader {
                        name: row.name.read(cx).value().to_string(),
                        value: match row.original.filter(|_| !row.dirty) {
                            Some(index) => ReplayValue::Original(index),
                            None => ReplayValue::Edited(row.value.read(cx).value().to_string()),
                        },
                    })
                    .collect(),
            ),
        };
        let result = self
            .source
            .upgrade()
            .and_then(|traffic| {
                let traffic = traffic.read(cx);
                (traffic.generation == self.generation)
                    .then(|| traffic.capture.read(cx).session.as_ref())
                    .flatten()
            })
            .ok_or("The capture session ended")
            .and_then(|session| session.replay(self.id, edits));
        match result {
            Ok(result) => {
                self.waiting = true;
                self.result = None;
                let source = self.source.clone();
                let generation = self.generation;
                cx.spawn(async move |this, cx| {
                    let result = result
                        .await
                        .unwrap_or_else(|_| Err("Capture stopped".into()));
                    let _ = this.update(cx, |this, cx| {
                        this.waiting = false;
                        this.result = Some(result.clone().map(|_| ()));
                        cx.notify();
                    });
                    let _ =
                        source.update(cx, |this, cx| this.replay_result(generation, result, cx));
                })
                .detach();
            }
            Err(error) => self.result = Some(Err(error.into())),
        }
        cx.notify();
    }
}

impl Render for Editor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(TitleBar::new().child(div().text_sm().child("Edit and resend")))
            .child(
                v_flex()
                    .id("traffic-editor-fields")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_4()
                    .gap_3()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().w(px(110.)).child(Input::new(&self.method)))
                            .child(div().flex_1().min_w_0().child(Input::new(&self.url))),
                    )
                    .child(
                        h_flex()
                            .justify_between()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Request headers"),
                            )
                            .child(
                                Button::new("traffic-editor-add")
                                    .outline()
                                    .small()
                                    .label("Add header")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        let header =
                                            Self::header(this.next_key, None, "", "", window, cx);
                                        this.next_key += 1;
                                        this.headers.push(header);
                                        cx.notify();
                                    })),
                            ),
                    )
                    .children(self.headers.iter().map(|header| {
                        let key = header.key;
                        h_flex()
                            .id(("traffic-editor-header", key))
                            .gap_2()
                            .child(div().w(px(195.)).child(Input::new(&header.name)))
                            .child(div().flex_1().min_w_0().child(Input::new(&header.value)))
                            .child(
                                Button::new(("traffic-editor-remove", key))
                                    .ghost()
                                    .small()
                                    .label("Remove")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.headers.retain(|header| header.key != key);
                                        cx.notify();
                                    })),
                            )
                    }))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Request body"),
                    )
                    .child(Textarea::new(&self.body))
                    .when(self.masked, |panel| {
                        panel.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(
                                    "Unchanged masked values are sent with their original values.",
                                ),
                        )
                    }),
            )
            .child(
                h_flex()
                    .p_4()
                    .gap_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(div().flex_1().text_sm().when_some(
                        self.result.as_ref(),
                        |row, result| {
                            match result {
                                Ok(()) => row.child("Repeated request captured"),
                                Err(error) => row
                                    .text_color(theme::palette(cx).danger)
                                    .child(error.clone()),
                            }
                        },
                    ))
                    .child(
                        Button::new("traffic-editor-close")
                            .outline()
                            .label("Close")
                            .on_click(|_, window, _| window.remove_window()),
                    )
                    .child(
                        Button::new("traffic-editor-send")
                            .primary()
                            .disabled(self.waiting)
                            .label(if self.waiting {
                                "Sending…"
                            } else {
                                "Send request"
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.send(cx))),
                    ),
            )
    }
}
