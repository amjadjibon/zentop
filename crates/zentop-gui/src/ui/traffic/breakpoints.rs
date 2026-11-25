//! The Breakpoints window: messages held by breakpoint rules, newest first,
//! with an editor, a countdown to the timeout, and Execute or Abort.
use std::time::Instant;

use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::FontWeight;
use zentop_proxy::{HeldEdits, HeldMessage};

use super::rules::ToolWindows;
use super::tools::{close_natively, header_editor, header_list, HeaderEditor, HeaderRows};
use super::*;

/// Brings Breakpoints forward whenever a new message is held, even when the
/// Traffic window is closed.
pub(super) fn watch(cx: &mut App) {
    cx.spawn(async move |cx| {
        let mut seen: (u64, Option<u64>) = (0, None);
        let mut revision = None;
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;
            cx.update(|cx| {
                let capture = global(cx).read(cx);
                if capture.generation != seen.0 {
                    seen = (capture.generation, None);
                    revision = None;
                }
                let Some(session) = &capture.session else {
                    return;
                };
                let current = session.held_revision();
                if revision == Some(current) {
                    return;
                }
                revision = Some(current);
                let newest = session.held_messages().first().map(|message| message.id);
                if let Some(id) = newest.filter(|id| seen.1.is_none_or(|old| *id > old)) {
                    seen.1 = Some(id);
                    open(cx);
                }
            });
        }
    })
    .detach();
}

/// Opens or focuses the Breakpoints window (⇧⌘B).
pub(super) fn open(cx: &mut App) {
    if !cx.has_global::<ToolWindows>() {
        cx.set_global(ToolWindows::default());
    }
    if let Some((handle, view)) = cx.global::<ToolWindows>().breakpoints.clone() {
        if view.upgrade().is_some()
            && handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
        {
            cx.activate(true);
            return;
        }
    }
    let options =
        super::super::window_options(size(px(940.), px(640.)), Some(size(px(760.), px(460.))), cx);
    if let Ok((handle, view)) = gpui_kit::open_window(options, cx, |window, cx| {
        window.set_window_title("Zentop Breakpoints");
        cx.new(|cx| BreakpointsWindow::new(window, cx))
    }) {
        cx.global_mut::<ToolWindows>().breakpoints = Some((handle, view.downgrade()));
    }
    cx.activate(true);
}

struct HeldEditor {
    message: HeldMessage,
    method: Entity<InputState>,
    url: Entity<InputState>,
    status: Entity<InputState>,
    body: Entity<TextareaState>,
}

pub(super) struct BreakpointsWindow {
    generation: u64,
    revision: Option<u64>,
    held: Vec<HeldMessage>,
    fetched: Instant,
    editor: Option<HeldEditor>,
    headers: Vec<HeaderEditor>,
    next_key: usize,
    result: Option<Result<String, String>>,
    _tick: Task<()>,
}

impl HeaderRows for BreakpointsWindow {
    fn header_rows(&mut self) -> &mut Vec<HeaderEditor> {
        &mut self.headers
    }
}

impl BreakpointsWindow {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let tick = cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            if this
                .update_in(cx, |this, window, cx| this.refresh(window, cx))
                .is_err()
            {
                break;
            }
        });
        let mut this = Self {
            generation: u64::MAX,
            revision: None,
            held: Vec::new(),
            fetched: Instant::now(),
            editor: None,
            headers: Vec::new(),
            next_key: 0,
            result: None,
            _tick: tick,
        };
        this.refresh(window, cx);
        this
    }

    fn session<'a>(&self, cx: &'a App) -> Option<&'a Session> {
        global(cx).read(cx).session.as_ref()
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let generation = global(cx).read(cx).generation;
        if generation != self.generation {
            self.generation = generation;
            self.revision = None;
            self.held.clear();
            self.editor = None;
            self.headers.clear();
        }
        let revision = self.session(cx).map(Session::held_revision);
        if revision.is_some() && revision != self.revision {
            self.revision = revision;
            self.held = self
                .session(cx)
                .map(Session::held_messages)
                .unwrap_or_default();
            self.fetched = Instant::now();
            let selected = self.editor.as_ref().map(|editor| editor.message.id);
            if !selected.is_some_and(|id| self.held.iter().any(|message| message.id == id)) {
                match self.held.first().map(|message| message.id) {
                    Some(id) => self.select(id, window, cx),
                    None => {
                        self.editor = None;
                        self.headers.clear();
                    }
                }
            }
        }
        if !self.held.is_empty() || revision.is_some() {
            cx.notify();
        }
    }

    fn select(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(message) = self
            .session(cx)
            .and_then(|session| session.held_message(id, false))
        else {
            return;
        };
        let mut text = |value: String| {
            let entity: Entity<InputState> =
                cx.new(|cx| InputState::new(window, cx).default_value(value));
            entity
        };
        let method = text(message.method.clone());
        let url = text(message.url.clone());
        let status = text(message.status.map(|s| s.to_string()).unwrap_or_default());
        let body_text = message.body.clone().unwrap_or_default();
        let editable = message.body_editable;
        let body = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(8)
                .placeholder(if editable {
                    "Body"
                } else {
                    "This body is sent unchanged"
                })
                .default_value(body_text)
        });
        self.headers = message
            .headers
            .iter()
            .enumerate()
            .map(|(index, (name, value))| {
                header_editor(index, Some(index), name, value, window, cx)
            })
            .collect();
        self.next_key = message.headers.len();
        self.editor = Some(HeldEditor {
            message,
            method,
            url,
            status,
            body,
        });
        self.result = None;
        cx.notify();
    }

    fn execute(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &self.editor else { return };
        let message = &editor.message;
        let value = |input: &Entity<InputState>| input.read(cx).value().trim().to_owned();
        let body = editor.body.read(cx).value().to_string();
        let body = (message.body_editable
            && body != message.body.clone().unwrap_or_default()
            && !(message.body.is_none() && body.is_empty()))
        .then_some(body);
        let mut edits = HeldEdits {
            headers: Some(self.headers.iter().map(|row| row.edit(cx)).collect()),
            body,
            preserve_masked_query: true,
            ..HeldEdits::default()
        };
        if message.response {
            let status = value(&editor.status);
            if Some(status.as_str()) != message.status.map(|s| s.to_string()).as_deref() {
                match status.parse() {
                    Ok(status) => edits.status = Some(status),
                    Err(_) => {
                        self.result = Some(Err("Enter a status code from 200 to 599".into()));
                        cx.notify();
                        return;
                    }
                }
            }
        } else {
            let method = value(&editor.method);
            let url = value(&editor.url);
            edits.method = (method != message.method).then_some(method);
            edits.url = (url != message.url).then_some(url);
        }
        let id = message.id;
        let result = self
            .session(cx)
            .ok_or("Capture stopped")
            .and_then(|session| session.execute_held(id, edits));
        self.result = Some(
            result
                .map(|()| "Executed".to_owned())
                .map_err(str::to_owned),
        );
        cx.notify();
    }

    fn abort(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.editor.as_ref().map(|editor| editor.message.id) else {
            return;
        };
        let aborted = self
            .session(cx)
            .is_some_and(|session| session.abort_held(id));
        self.result = Some(if aborted {
            Ok("Aborted; the client received 502".into())
        } else {
            Err("The message is no longer held".into())
        });
        cx.notify();
    }

    fn remaining(&self, message: &HeldMessage) -> String {
        let left = message.remaining.saturating_sub(self.fetched.elapsed());
        let seconds = left.as_secs();
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }

    fn list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.editor.as_ref().map(|editor| editor.message.id);
        let muted = cx.theme().muted_foreground;
        v_flex()
            .id("breakpoints-list")
            .w(px(300.))
            .h_full()
            .flex_shrink_0()
            .overflow_y_scroll()
            .bg(theme::palette(cx).card)
            .border_r_1()
            .border_color(cx.theme().border)
            .when(self.held.is_empty(), |list| {
                list.child(
                    v_flex()
                        .p_4()
                        .gap_2()
                        .items_center()
                        .child(Icon::new(IconName::OctagonPause).size_6().text_color(muted))
                        .child(div().text_sm().child("Nothing is held"))
                        .child(div().text_xs().text_color(muted).text_center().child(
                            if breakpoints_enabled(cx) {
                                "Messages matching a breakpoint rule appear here."
                            } else {
                                "Breakpoints are off. Press ⌘B in Traffic to turn them on."
                            },
                        )),
                )
            })
            .children(self.held.iter().map(|message| {
                let id = message.id;
                h_flex()
                    .id(("breakpoints-row", id as usize))
                    .p_2()
                    .gap_2()
                    .items_start()
                    .cursor_pointer()
                    .border_b_1()
                    .border_color(cx.theme().table_row_border)
                    .hover(|row| row.bg(cx.theme().table_hover))
                    .when(selected == Some(id), |row| {
                        row.bg(cx.theme().table_active)
                            .border_l_2()
                            .border_color(cx.theme().primary)
                    })
                    .on_click(cx.listener(move |this, _, window, cx| this.select(id, window, cx)))
                    .child(
                        div()
                            .flex_shrink_0()
                            .px_1()
                            .rounded_sm()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().background)
                            .bg(if message.response {
                                theme::palette(cx).warning
                            } else {
                                cx.theme().primary
                            })
                            .child(if message.response { "RES" } else { "REQ" }),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_xs()
                                    .truncate()
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .child(match message.status {
                                        Some(status) if message.response => {
                                            format!("{status} {} {}", message.method, message.url)
                                        }
                                        _ => format!("{} {}", message.method, message.url),
                                    }),
                            )
                            .child(div().text_xs().text_color(muted).truncate().child(format!(
                                "{} · continues in {}",
                                message.rule,
                                self.remaining(message)
                            ))),
                    )
            }))
    }

    fn editor_view(&self, editor: &HeldEditor, cx: &mut Context<Self>) -> impl IntoElement {
        let message = &editor.message;
        let muted = cx.theme().muted_foreground;
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(if message.response {
                                "Held response"
                            } else {
                                "Held request"
                            }),
                    )
                    .child(div().text_xs().text_color(muted).child(format!(
                                "{}{} · continues unmodified in {}",
                                message.rule,
                                message
                                    .flow
                                    .map(|flow| format!(" · flow #{flow}"))
                                    .unwrap_or_default(),
                                self.remaining(message)
                            ))),
            )
            .child(if message.response {
                h_flex()
                    .gap_2()
                    .child(div().w(px(110.)).child(Input::new(&editor.status)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .truncate()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_color(muted)
                            .child(format!("{} {}", message.method, message.url)),
                    )
                    .into_any_element()
            } else {
                h_flex()
                    .gap_2()
                    .child(div().w(px(110.)).child(Input::new(&editor.method)))
                    .child(div().flex_1().min_w_0().child(Input::new(&editor.url)))
                    .into_any_element()
            })
            .child(
                h_flex()
                    .justify_between()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Headers"))
                    .child(
                        Button::new("breakpoints-add-header")
                            .outline()
                            .small()
                            .label("Add header")
                            .on_click(cx.listener(|this, _, window, cx| {
                                let header = header_editor(this.next_key, None, "", "", window, cx);
                                this.next_key += 1;
                                this.headers.push(header);
                                cx.notify();
                            })),
                    ),
            )
            .children(header_list(&self.headers, "breakpoints-header", cx))
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Body"))
            .when_some(message.body_note, |panel, note| {
                panel.child(div().text_xs().text_color(muted).child(note))
            })
            .child(Textarea::new(&editor.body).disabled(!message.body_editable))
            .child(div().text_xs().text_color(muted).child(
                "Masked values you leave unchanged are sent with their original values. \
                 Abort answers the client with 502.",
            ))
    }
}

impl Render for BreakpointsWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let editor = match self.editor.take() {
            Some(editor) => {
                let view = self.editor_view(&editor, cx).into_any_element();
                self.editor = Some(editor);
                view
            }
            None => v_flex()
                .size_full()
                .justify_center()
                .items_center()
                .text_color(cx.theme().muted_foreground)
                .child("Select a held message")
                .into_any_element(),
        };
        let holding = self.editor.is_some();
        let running = self.session(cx).is_some();
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                TitleBar::new()
                    .bg(theme::palette(cx).card)
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().text_sm().child("Breakpoints"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(match (running, self.held.len()) {
                                        (false, _) => "Capture stopped".to_owned(),
                                        (_, 1) => "1 message held".to_owned(),
                                        (_, count) => format!("{count} messages held"),
                                    }),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .child(self.list(cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .id("breakpoints-editor")
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .p_4()
                                    .child(editor),
                            )
                            .child(
                                h_flex()
                                    .p_3()
                                    .gap_2()
                                    .border_t_1()
                                    .border_color(cx.theme().border)
                                    .child(div().flex_1().min_w_0().text_sm().when_some(
                                        self.result.as_ref(),
                                        |row, result| {
                                            match result {
                                                Ok(message) => row
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(message.clone()),
                                                Err(error) => row
                                                    .text_color(theme::palette(cx).danger)
                                                    .child(error.clone()),
                                            }
                                        },
                                    ))
                                    .child(
                                        Button::new("breakpoints-close")
                                            .outline()
                                            .label("Close")
                                            .on_click(|_, window, _| {
                                                if !close_natively(window) {
                                                    window.remove_window();
                                                }
                                            }),
                                    )
                                    .child(
                                        Button::new("breakpoints-abort")
                                            .outline()
                                            .label("Abort")
                                            .disabled(!holding)
                                            .on_click(cx.listener(|this, _, _, cx| this.abort(cx))),
                                    )
                                    .child(
                                        Button::new("breakpoints-execute")
                                            .primary()
                                            .label("Execute")
                                            .disabled(!holding)
                                            .on_click(
                                                cx.listener(|this, _, _, cx| this.execute(cx)),
                                            ),
                                    ),
                            ),
                    ),
            )
    }
}
