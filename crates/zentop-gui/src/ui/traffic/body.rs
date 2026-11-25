//! The body viewer: Auto, JSON, Text, Form, Hex and Image formats, built off
//! the UI thread from the shared 256 KB preview and rendered virtualized.
use std::collections::HashSet;

use super::*;
use gpui_kit::{ImageFormat, ObjectFit, StyledImage};

const MAX_LINE_CHARS: usize = 2_000;
const HEX_LIMIT: usize = 64 * 1024;
const LINE_HEIGHT: f32 = 18.;
const MAX_DEPTH: u16 = 256;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum BodyFormat {
    #[default]
    Auto,
    Json,
    Text,
    Form,
    Hex,
    Image,
}

impl BodyFormat {
    pub(super) const ALL: [Self; 6] = [
        Self::Auto,
        Self::Json,
        Self::Text,
        Self::Form,
        Self::Hex,
        Self::Image,
    ];

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Json => "JSON",
            Self::Text => "Text",
            Self::Form => "Form",
            Self::Hex => "Hex",
            Self::Image => "Image",
        }
    }
}

/// One rendered line of a JSON document.
#[derive(Debug, PartialEq)]
pub(super) struct JsonLine {
    pub depth: u16,
    pub key: Option<SharedString>,
    pub token: JsonToken,
    pub comma: bool,
}

#[derive(Debug, PartialEq)]
pub(super) enum JsonToken {
    Open {
        array: bool,
        /// The index of the matching closing line.
        close: usize,
        items: usize,
    },
    Close {
        array: bool,
    },
    String(SharedString),
    Number(SharedString),
    Literal(SharedString),
}

/// A JSON document as foldable lines, in source order.
#[derive(Debug, Default)]
pub(super) struct JsonTree {
    pub lines: Vec<JsonLine>,
}

struct Parser<'a> {
    text: &'a str,
    pos: usize,
    lines: Vec<JsonLine>,
}

impl Parser<'_> {
    fn bytes(&self) -> &[u8] {
        self.text.as_bytes()
    }

    fn whitespace(&mut self) {
        while self
            .bytes()
            .get(self.pos)
            .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\r'))
        {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes().get(self.pos).copied()
    }

    fn string(&mut self) -> Option<SharedString> {
        let start = self.pos;
        if self.peek()? != b'"' {
            return None;
        }
        self.pos += 1;
        loop {
            match *self.bytes().get(self.pos)? {
                b'"' => break,
                b'\\' => self.pos += 2,
                byte if byte < 0x20 => return None,
                _ => self.pos += 1,
            }
        }
        self.pos += 1;
        Some(clip(&self.text[start..self.pos]))
    }

    fn value(&mut self, depth: u16, key: Option<SharedString>) -> Option<()> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.whitespace();
        let token = match self.peek()? {
            open @ (b'{' | b'[') => {
                let array = open == b'[';
                let close_byte = if array { b']' } else { b'}' };
                self.pos += 1;
                let index = self.lines.len();
                self.lines.push(JsonLine {
                    depth,
                    key,
                    token: JsonToken::Open {
                        array,
                        close: 0,
                        items: 0,
                    },
                    comma: false,
                });
                let mut items = 0;
                self.whitespace();
                if self.peek()? == close_byte {
                    self.pos += 1;
                } else {
                    loop {
                        let key = if array {
                            None
                        } else {
                            self.whitespace();
                            let key = self.string()?;
                            self.whitespace();
                            if self.peek()? != b':' {
                                return None;
                            }
                            self.pos += 1;
                            Some(key)
                        };
                        self.value(depth + 1, key)?;
                        items += 1;
                        self.whitespace();
                        match self.peek()? {
                            b',' => {
                                self.pos += 1;
                                self.lines.last_mut()?.comma = true;
                            }
                            byte if byte == close_byte => {
                                self.pos += 1;
                                break;
                            }
                            _ => return None,
                        }
                    }
                }
                let close = self.lines.len();
                self.lines.push(JsonLine {
                    depth,
                    key: None,
                    token: JsonToken::Close { array },
                    comma: false,
                });
                self.lines[index].token = JsonToken::Open {
                    array,
                    close,
                    items,
                };
                return Some(());
            }
            b'"' => JsonToken::String(self.string()?),
            b'-' | b'0'..=b'9' => {
                let start = self.pos;
                while self.peek().is_some_and(|byte| {
                    matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                }) {
                    self.pos += 1;
                }
                JsonToken::Number(self.text[start..self.pos].to_owned().into())
            }
            _ => {
                let literal = ["true", "false", "null"]
                    .into_iter()
                    .find(|literal| self.text[self.pos..].starts_with(literal))?;
                self.pos += literal.len();
                JsonToken::Literal(literal.into())
            }
        };
        self.lines.push(JsonLine {
            depth,
            key,
            token,
            comma: false,
        });
        Some(())
    }
}

fn clip(text: &str) -> SharedString {
    match text.char_indices().nth(MAX_LINE_CHARS) {
        Some((end, _)) => format!("{}…", &text[..end]).into(),
        None => text.to_owned().into(),
    }
}

impl JsonTree {
    /// Parses one JSON document, or `None` if the text isn't exactly one.
    pub(super) fn parse(text: &str) -> Option<Self> {
        let start = text.trim_start();
        if !start.starts_with(['{', '[']) {
            return None;
        }
        let mut parser = Parser {
            text,
            pos: 0,
            lines: Vec::new(),
        };
        parser.value(0, None)?;
        parser.whitespace();
        (parser.pos == text.len()).then_some(Self {
            lines: parser.lines,
        })
    }

    /// The line indices shown when the given opening lines are folded.
    pub(super) fn visible(&self, folded: &HashSet<usize>) -> Vec<usize> {
        let mut lines = Vec::with_capacity(self.lines.len());
        let mut index = 0;
        while index < self.lines.len() {
            lines.push(index);
            index = match self.lines[index].token {
                // Empty containers and folded ones hide everything to their close.
                JsonToken::Open { close, items, .. } if items == 0 || folded.contains(&index) => {
                    close + 1
                }
                _ => index + 1,
            };
        }
        lines
    }
}

/// Every representation of one body, prepared in the background.
pub(super) struct BodyView {
    pub note: Option<SharedString>,
    pub text: Arc<[SharedString]>,
    pub hex: Arc<[SharedString]>,
    pub json: Option<Arc<JsonTree>>,
    pub form: Option<Arc<[(SharedString, SharedString)]>>,
    pub image: Option<Arc<gpui_kit::Image>>,
    pub limited: bool,
    pub auto: BodyFormat,
}

impl BodyView {
    fn note(note: impl Into<SharedString>) -> Self {
        Self {
            note: Some(note.into()),
            text: Arc::new([]),
            hex: Arc::new([]),
            json: None,
            form: None,
            image: None,
            limited: false,
            auto: BodyFormat::Text,
        }
    }

    pub(super) fn build(body: &Body, headers: &Headers) -> Self {
        if let Body::Skipped(reason) = body {
            return Self::note(*reason);
        }
        if body.kept().is_empty() {
            return Self::note("Empty body");
        }
        let header = |wanted: &str| {
            headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(wanted))
                .map(|(_, value)| value.to_ascii_lowercase())
        };
        let content_type = header("content-type").unwrap_or_default();
        let image = image(body, &content_type, header("content-encoding").as_deref());
        let preview = match zentop_proxy::preview(body, headers) {
            Ok(preview) => preview,
            Err(error) => {
                let mut view = Self::note(error.to_string());
                view.hex = hex(&body.kept()[..body.kept().len().min(HEX_LIMIT)]);
                view.auto = BodyFormat::Hex;
                view.image = image;
                if view.image.is_some() {
                    view.auto = BodyFormat::Image;
                }
                return view;
            }
        };
        let lines = |text: &str| -> Arc<[SharedString]> { text.lines().map(clip).collect() };
        let (text, hex, json, form) = if preview.binary {
            (Arc::from([]), lines(&preview.text), None, None)
        } else {
            let bytes = preview.text.as_bytes();
            (
                lines(&preview.text),
                hex(&bytes[..bytes.len().min(HEX_LIMIT)]),
                JsonTree::parse(&preview.text).map(Arc::new),
                parse::form(&preview.text).map(|pairs| {
                    pairs
                        .into_iter()
                        .map(|(name, value)| (name.into(), value.into()))
                        .collect()
                }),
            )
        };
        let auto = if image.is_some() {
            BodyFormat::Image
        } else if json.is_some() {
            BodyFormat::Json
        } else if form.is_some() && content_type.starts_with("application/x-www-form-urlencoded") {
            BodyFormat::Form
        } else if preview.binary {
            BodyFormat::Hex
        } else {
            BodyFormat::Text
        };
        Self {
            note: None,
            text,
            hex,
            json,
            form,
            image,
            limited: preview.limited,
            auto,
        }
    }

    pub(super) fn available(&self, format: BodyFormat) -> bool {
        match format {
            BodyFormat::Auto => true,
            BodyFormat::Json => self.json.is_some(),
            BodyFormat::Text => !self.text.is_empty(),
            BodyFormat::Form => self.form.is_some(),
            BodyFormat::Hex => !self.hex.is_empty(),
            BodyFormat::Image => self.image.is_some(),
        }
    }

    /// The format to show for a choice, falling back to Auto's choice.
    pub(super) fn resolve(&self, chosen: BodyFormat) -> BodyFormat {
        if chosen != BodyFormat::Auto && self.available(chosen) {
            chosen
        } else {
            self.auto
        }
    }
}

/// A bounded image preview, only for complete, unencoded raster images.
fn image(body: &Body, content_type: &str, encoding: Option<&str>) -> Option<Arc<gpui_kit::Image>> {
    let Body::Captured(bytes) = body else {
        return None;
    };
    if encoding.is_some_and(|encoding| encoding.trim() != "identity") {
        return None;
    }
    let sniffed = match bytes.as_slice() {
        [0x89, b'P', b'N', b'G', ..] => Some(ImageFormat::Png),
        [0xff, 0xd8, 0xff, ..] => Some(ImageFormat::Jpeg),
        [b'G', b'I', b'F', b'8', ..] => Some(ImageFormat::Gif),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some(ImageFormat::Webp),
        [b'B', b'M', ..] => Some(ImageFormat::Bmp),
        _ => None,
    };
    let declared = match content_type.split(';').next().unwrap_or_default().trim() {
        "image/png" => Some(ImageFormat::Png),
        "image/jpeg" | "image/jpg" => Some(ImageFormat::Jpeg),
        "image/gif" => Some(ImageFormat::Gif),
        "image/webp" => Some(ImageFormat::Webp),
        "image/bmp" => Some(ImageFormat::Bmp),
        _ => None,
    };
    // Both must agree, so mislabelled data never reaches the image decoder.
    let format = sniffed.filter(|format| declared.is_none_or(|declared| declared == *format))?;
    Some(Arc::new(gpui_kit::Image::from_bytes(
        format,
        bytes.as_slice().to_vec(),
    )))
}

pub(super) fn hex(bytes: &[u8]) -> Arc<[SharedString]> {
    use std::fmt::Write as _;
    bytes
        .chunks(16)
        .enumerate()
        .map(|(row, chunk)| {
            let mut line = format!("{:08x} ", row * 16);
            for (index, byte) in chunk.iter().enumerate() {
                if index == 8 {
                    line.push(' ');
                }
                let _ = write!(line, " {byte:02x}");
            }
            let padding = (16 - chunk.len()) * 3 + usize::from(chunk.len() <= 8);
            line.extend(std::iter::repeat_n(' ', padding + 2));
            line.extend(chunk.iter().map(|&byte| {
                if byte.is_ascii_graphic() || byte == b' ' {
                    byte as char
                } else {
                    '.'
                }
            }));
            line.into()
        })
        .collect()
}

impl Traffic {
    pub(super) fn body_viewer(
        &self,
        side: usize,
        view: Option<&BodyView>,
        body: &Body,
        cx: &Context<Self>,
    ) -> Div {
        let Some(view) = view else {
            return note_panel("Preparing preview…", cx);
        };
        let chosen = self.panes[side].format;
        let format = view.resolve(chosen);
        let entity = cx.entity();
        let available: Vec<_> = BodyFormat::ALL
            .into_iter()
            .map(|format| (format, view.available(format)))
            .collect();
        let picker = Button::new(("traffic-body-format", side))
            .ghost()
            .xsmall()
            .label(if chosen == BodyFormat::Auto {
                format!("Auto · {}", format.name())
            } else {
                format.name().to_owned()
            })
            .dropdown_caret(true)
            .disabled(view.note.is_some() && view.hex.is_empty())
            .dropdown_menu(move |mut menu, _, _| {
                for (option, enabled) in available.iter().copied() {
                    let entity = entity.clone();
                    menu = menu.item(
                        PopupMenuItem::new(option.name())
                            .checked(chosen == option)
                            .disabled(!enabled)
                            .on_click(move |_, _, cx| {
                                entity.update(cx, |this, cx| {
                                    this.panes[side].format = option;
                                    cx.notify();
                                })
                            }),
                    );
                }
                menu
            });
        let copy = {
            let body = body.clone();
            Button::new(("traffic-copy-body", side))
                .ghost()
                .xsmall()
                .icon(IconName::Copy)
                .label("Copy body")
                .disabled(body.kept().is_empty())
                .on_click(move |_, _, cx| {
                    if let Some((_, value)) = zentop_proxy::copy_body(&body) {
                        cx.write_to_clipboard(ClipboardItem::new_string(value));
                    }
                })
        };
        let bar = h_flex()
            .gap_1()
            .flex_shrink_0()
            .child(picker)
            .child(copy)
            .child(div().flex_1())
            .when(view.limited, |row| {
                row.child(
                    div()
                        .text_color(theme::palette(cx).warning)
                        .child("Preview limited to 256 KB"),
                )
            });
        let content = match (format, &view.note) {
            (BodyFormat::Image, _) if view.image.is_some() => {
                let image = view.image.clone().unwrap();
                div()
                    .id(("traffic-image", side))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_2()
                    .child(
                        gpui_kit::img(image)
                            .max_w_full()
                            .max_h(px(320.))
                            .object_fit(ObjectFit::Contain),
                    )
                    .into_any_element()
            }
            (BodyFormat::Hex, _) if !view.hex.is_empty() => {
                lines(("traffic-hex", side), view.hex.clone(), false, cx).into_any_element()
            }
            (_, Some(note)) => note_panel(note.clone(), cx).into_any_element(),
            (BodyFormat::Json, _) if view.json.is_some() => self
                .json(side, view.json.clone().unwrap(), cx)
                .into_any_element(),
            (BodyFormat::Form, _) if view.form.is_some() => {
                pairs_list(("traffic-form", side), view.form.clone().unwrap(), cx)
                    .into_any_element()
            }
            _ => lines(("traffic-text", side), view.text.clone(), true, cx).into_any_element(),
        };
        v_flex()
            .size_full()
            .min_h_0()
            .gap_1()
            .child(bar)
            .child(content)
    }

    fn json(&self, side: usize, tree: Arc<JsonTree>, cx: &Context<Self>) -> impl IntoElement {
        let visible: Arc<[usize]> = tree.visible(&self.panes[side].folded).into();
        let entity = cx.entity();
        let colors = theme::palette(cx).charts;
        let muted = cx.theme().muted_foreground;
        let mono = cx.theme().mono_font_family.clone();
        let gutter = digits(tree.lines.len());
        uniform_list(
            ("traffic-json", side),
            visible.len(),
            move |range, _, cx| {
                let folded = &entity.read(cx).panes[side].folded;
                range
                    .filter_map(|row| {
                        let index = *visible.get(row)?;
                        let line = tree.lines.get(index)?;
                        let is_folded = folded.contains(&index);
                        let mut text = h_flex()
                            .min_w_0()
                            .overflow_hidden()
                            .pl(px(f32::from(line.depth) * 14.))
                            .whitespace_nowrap();
                        if let Some(key) = &line.key {
                            text = text
                                .child(div().text_color(colors[0]).child(key.clone()))
                                .child(div().text_color(muted).child(": "));
                        }
                        let comma = |line: &JsonLine| if line.comma { "," } else { "" };
                        text = match &line.token {
                            JsonToken::Open {
                                array,
                                close,
                                items,
                            } => {
                                let (open, end) = if *array { ("[", "]") } else { ("{", "}") };
                                if *items == 0 {
                                    text.child(div().text_color(muted).child(format!(
                                        "{open}{end}{}",
                                        comma(&tree.lines[*close])
                                    )))
                                } else if is_folded {
                                    text.child(div().text_color(muted).child(format!(
                                        "{open} … {end}{}",
                                        comma(&tree.lines[*close])
                                    )))
                                    .child(
                                        div().pl_2().text_color(muted).child(match items {
                                            1 => "1 item".to_owned(),
                                            n => format!("{n} items"),
                                        }),
                                    )
                                } else {
                                    text.child(div().text_color(muted).child(open))
                                }
                            }
                            JsonToken::Close { array } => {
                                text.child(div().text_color(muted).child(format!(
                                    "{}{}",
                                    if *array { "]" } else { "}" },
                                    comma(line)
                                )))
                            }
                            token => {
                                let (value, color) = match token {
                                    JsonToken::String(value) => (value, colors[1]),
                                    JsonToken::Number(value) => (value, colors[2]),
                                    JsonToken::Literal(value) => (value, colors[3]),
                                    _ => unreachable!(),
                                };
                                text.child(div().text_color(color).child(value.clone()))
                                    .child(div().text_color(muted).child(comma(line)))
                            }
                        };
                        let foldable =
                            matches!(line.token, JsonToken::Open { items, .. } if items > 0);
                        let toggle = entity.clone();
                        Some(
                            h_flex()
                                .id(("traffic-json-line", index))
                                .h(px(LINE_HEIGHT))
                                .w_full()
                                .text_xs()
                                .font_family(mono.clone())
                                .child(
                                    div()
                                        .w(px(gutter))
                                        .flex_shrink_0()
                                        .pr_2()
                                        .text_right()
                                        .text_color(muted)
                                        .child((index + 1).to_string()),
                                )
                                .child(
                                    div().w(px(12.)).flex_shrink_0().text_color(muted).when(
                                        foldable,
                                        |toggle| {
                                            toggle.child(if is_folded { "▸" } else { "▾" })
                                        },
                                    ),
                                )
                                .child(text)
                                .when(foldable, |row| {
                                    row.cursor_pointer().on_click(move |_, _, cx| {
                                        toggle.update(cx, |this, cx| {
                                            let folded = &mut this.panes[side].folded;
                                            if !folded.remove(&index) {
                                                folded.insert(index);
                                            }
                                            cx.notify();
                                        })
                                    })
                                }),
                        )
                    })
                    .collect::<Vec<_>>()
            },
        )
        .flex_1()
        .min_h_0()
        .w_full()
    }
}

fn digits(count: usize) -> f32 {
    (count.max(1).ilog10() + 1) as f32 * 7. + 12.
}

pub(super) fn note_panel(note: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .p_2()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(note.into())
}

/// Virtualized monospace lines, optionally numbered.
pub(super) fn lines(
    id: impl Into<gpui_kit::ElementId>,
    lines: Arc<[SharedString]>,
    numbered: bool,
    cx: &App,
) -> impl IntoElement {
    let mono = cx.theme().mono_font_family.clone();
    let muted = cx.theme().muted_foreground;
    let gutter = digits(lines.len());
    uniform_list(id, lines.len(), move |range, _, _| {
        range
            .filter_map(|index| {
                let line = lines.get(index)?.clone();
                Some(
                    h_flex()
                        .h(px(LINE_HEIGHT))
                        .w_full()
                        .text_xs()
                        .font_family(mono.clone())
                        .when(numbered, |row| {
                            row.child(
                                div()
                                    .w(px(gutter))
                                    .flex_shrink_0()
                                    .pr_2()
                                    .text_right()
                                    .text_color(muted)
                                    .child((index + 1).to_string()),
                            )
                        })
                        .child(div().min_w_0().pl_1().whitespace_nowrap().child(line)),
                )
            })
            .collect::<Vec<_>>()
    })
    .flex_1()
    .min_h_0()
    .w_full()
}

/// A virtualized two-column key/value table.
pub(super) fn pairs_list(
    id: impl Into<gpui_kit::ElementId>,
    pairs: Arc<[(SharedString, SharedString)]>,
    cx: &App,
) -> impl IntoElement {
    let mono = cx.theme().mono_font_family.clone();
    let border = cx.theme().table_row_border;
    let muted = cx.theme().muted_foreground;
    uniform_list(id, pairs.len(), move |range, _, _| {
        range
            .filter_map(|index| {
                let (name, value) = pairs.get(index)?.clone();
                Some(
                    h_flex()
                        .h(px(22.))
                        .w_full()
                        .gap_2()
                        .px_2()
                        .text_xs()
                        .border_b_1()
                        .border_color(border)
                        .child(
                            div()
                                .w(px(170.))
                                .flex_shrink_0()
                                .truncate()
                                .text_color(muted)
                                .child(name),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family(mono.clone())
                                .child(value),
                        ),
                )
            })
            .collect::<Vec<_>>()
    })
    .flex_1()
    .min_h_0()
    .w_full()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(text: &str) -> JsonTree {
        JsonTree::parse(text).expect("valid JSON")
    }

    #[test]
    fn builds_json_lines_in_source_order_with_commas_and_matching_brackets() {
        let tree = tree(r#"{"z": 1, "a": [true, null, "s\"q"], "e": {}, "n": -1.5e3}"#);
        let render: Vec<_> = tree
            .lines
            .iter()
            .map(|line| {
                let key = line.key.as_ref().map_or("", |key| key.as_ref());
                let token = match &line.token {
                    JsonToken::Open { array, items, .. } => {
                        format!("{}{items}", if *array { "[" } else { "{" })
                    }
                    JsonToken::Close { array } => (if *array { "]" } else { "}" }).into(),
                    JsonToken::String(value)
                    | JsonToken::Number(value)
                    | JsonToken::Literal(value) => value.to_string(),
                };
                format!(
                    "{}{key}{token}{}",
                    " ".repeat(line.depth as usize),
                    if line.comma { "," } else { "" }
                )
            })
            .collect();
        assert_eq!(
            render,
            [
                "{4",
                " \"z\"1,",
                " \"a\"[3",
                "  true,",
                "  null,",
                "  \"s\\\"q\"",
                " ],",
                " \"e\"{0",
                " },",
                " \"n\"-1.5e3",
                "}",
            ]
        );
        assert!(matches!(
            tree.lines[0].token,
            JsonToken::Open { close: 10, .. }
        ));
        assert!(matches!(
            tree.lines[2].token,
            JsonToken::Open { close: 6, .. }
        ));
    }

    #[test]
    fn folding_hides_children_through_the_closing_line() {
        let tree = tree("[{\"a\": [1, 2]}, 3]");
        assert_eq!(tree.visible(&HashSet::new()), (0..9).collect::<Vec<_>>());
        // Folding the inner array hides its items and closing bracket.
        assert_eq!(tree.visible(&HashSet::from([2])), [0, 1, 2, 6, 7, 8]);
        // Folding an outer level also hides the folded inner level.
        assert_eq!(tree.visible(&HashSet::from([1, 2])), [0, 1, 7, 8]);
        assert_eq!(tree.visible(&HashSet::from([0])), [0]);
        // Scalar lines can't fold.
        assert_eq!(tree.visible(&HashSet::from([3])).len(), 9);
        // Empty containers render on one line, without their closing line.
        let empty = super::tests::tree("{\"a\": {}, \"b\": []}");
        assert_eq!(empty.visible(&HashSet::new()), [0, 1, 3, 5]);
    }

    #[test]
    fn rejects_invalid_or_partial_json_and_deep_nesting() {
        for text in [
            "",
            "plain",
            "{",
            "{\"a\" 1}",
            "[1,]x",
            "{\"a\":1} {\"b\":2}",
            "[tru]",
            "\"text\"",
            "[\"raw\ncontrol\"]",
        ] {
            assert!(JsonTree::parse(text).is_none(), "{text:?}");
        }
        let deep = format!("{}{}", "[".repeat(300), "]".repeat(300));
        assert!(JsonTree::parse(&deep).is_none());
        let fine = format!("{}{}", "[".repeat(100), "]".repeat(100));
        assert!(JsonTree::parse(&fine).is_some());
        assert_eq!(tree("  [ ]\n").lines.len(), 2);
    }

    #[test]
    fn auto_format_prefers_json_form_hex_or_text() {
        let headers = |kind: &str| vec![("content-type".to_owned(), kind.to_owned())];
        let body = |bytes: &[u8]| Body::Captured(Arc::new(bytes.to_vec()));
        let json = BodyView::build(&body(br#"{"ok":true}"#), &headers("application/json"));
        assert_eq!(json.auto, BodyFormat::Json);
        assert_eq!(json.resolve(BodyFormat::Image), BodyFormat::Json);
        assert_eq!(json.resolve(BodyFormat::Hex), BodyFormat::Hex);
        let form = BodyView::build(
            &body(b"a=1&b=two"),
            &headers("application/x-www-form-urlencoded"),
        );
        assert_eq!(form.auto, BodyFormat::Form);
        let text = BodyView::build(&body(b"a=1&b=two"), &headers("text/plain"));
        assert_eq!(text.auto, BodyFormat::Text);
        assert!(text.available(BodyFormat::Form));
        let binary = BodyView::build(&body(&[0, 1, 2, 255]), &headers("application/x"));
        assert_eq!(binary.auto, BodyFormat::Hex);
        // A PNG signature labelled as JPEG is not decoded as an image.
        let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        let mislabelled = BodyView::build(&body(&png), &headers("image/jpeg"));
        assert!(mislabelled.image.is_none());
        assert_eq!(
            BodyView::build(&body(&png), &headers("image/png")).auto,
            BodyFormat::Image
        );
        let skipped = BodyView::build(&Body::Skipped("memory limit"), &Vec::new());
        assert_eq!(skipped.note.as_deref(), Some("memory limit"));
        assert_eq!(
            hex(b"AB\x00")[0].as_ref(),
            format!("00000000  41 42 00{}AB.", " ".repeat(42))
        );
    }
}
