//! The Rules window: create, edit, enable, reorder and delete breakpoints,
//! Map Local, Map Remote and rewrite rules. Rules are saved in the GUI
//! settings and applied to the running capture as soon as they are saved.
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::{FontWeight, WeakEntity};
use zentop_proxy::rules::{Action, RewriteAction, RuleSide};
use zentop_proxy::{Rule, RuleKind};

use super::tools::close_natively;
use super::*;

/// Open Rules and Breakpoints windows, reused instead of duplicated.
#[derive(Default)]
pub(super) struct ToolWindows {
    pub(super) rules: Option<(gpui_kit::AnyWindowHandle, WeakEntity<RulesWindow>)>,
    pub(super) breakpoints: Option<(
        gpui_kit::AnyWindowHandle,
        WeakEntity<super::breakpoints::BreakpointsWindow>,
    )>,
}
impl Global for ToolWindows {}

/// Opens the Rules window, optionally with an unsaved draft or a selected rule.
pub(super) fn open(draft: Option<Rule>, select: Option<u32>, cx: &mut App) {
    super::super::companion::close(cx);
    if !cx.has_global::<ToolWindows>() {
        cx.set_global(ToolWindows::default());
    }
    if let Some((handle, view)) = cx.global::<ToolWindows>().rules.clone() {
        let shown = handle.update(cx, |_, window, cx| {
            let _ = view.update(cx, |this, cx| this.show(draft.clone(), select, window, cx));
            window.activate_window();
        });
        if shown.is_ok() && view.upgrade().is_some() {
            cx.activate(true);
            return;
        }
    }
    let options =
        super::super::window_options(size(px(980.), px(660.)), Some(size(px(780.), px(480.))), cx);
    if let Ok((handle, view)) = gpui_kit::open_window(options, cx, |window, cx| {
        window.set_window_title("Zentop Rules");
        cx.new(|cx| {
            let mut this = RulesWindow::new(cx);
            this.show(draft, select, window, cx);
            this
        })
    }) {
        cx.global_mut::<ToolWindows>().rules = Some((handle, view.downgrade()));
    }
    cx.activate(true);
}

/// A new rule of `kind`, matching `url`.
pub(super) fn template(kind: RuleKind, url: String, host: Option<&str>) -> Rule {
    let action = match kind {
        RuleKind::Breakpoint => Action::Breakpoint {
            request: true,
            response: true,
        },
        RuleKind::MapLocal => Action::MapLocal {
            path: String::new(),
            status: 200,
            content_type: None,
            headers: Vec::new(),
        },
        RuleKind::MapRemote => Action::MapRemote {
            scheme: None,
            host: host.map(str::to_owned),
            port: None,
            path: None,
            keep_query: true,
        },
        RuleKind::Rewrite => Action::Rewrite {
            actions: vec![RewriteAction::SetHeader {
                side: RuleSide::Response,
                name: String::new(),
                value: String::new(),
            }],
        },
    };
    let target = url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .filter(|host| !host.is_empty() && *host != "*")
        .map(|host| format!(" · {host}"))
        .unwrap_or_default();
    Rule {
        id: 0,
        name: format!("{}{target}", kind.name()),
        enabled: true,
        method: None,
        url,
        app: None,
        action,
    }
}

pub(super) fn kind_icon(kind: RuleKind) -> IconName {
    match kind {
        RuleKind::Breakpoint => IconName::OctagonPause,
        RuleKind::MapLocal => IconName::FileInput,
        RuleKind::MapRemote => IconName::ArrowRightLeft,
        RuleKind::Rewrite => IconName::Replace,
    }
}

/// The colour that marks each kind in lists and badges.
pub(super) fn kind_color(kind: RuleKind, cx: &App) -> gpui_kit::Hsla {
    let palette = theme::palette(cx);
    match kind {
        RuleKind::Breakpoint => palette.danger,
        RuleKind::MapLocal => palette.running,
        RuleKind::MapRemote => cx.theme().primary,
        RuleKind::Rewrite => palette.warning,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActionKind {
    AddHeader,
    SetHeader,
    RemoveHeader,
    Status,
    ReplaceBody,
    SetQuery,
    RemoveQuery,
}

impl ActionKind {
    const ALL: [Self; 7] = [
        Self::AddHeader,
        Self::SetHeader,
        Self::RemoveHeader,
        Self::Status,
        Self::ReplaceBody,
        Self::SetQuery,
        Self::RemoveQuery,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::AddHeader => "Add header",
            Self::SetHeader => "Replace header",
            Self::RemoveHeader => "Remove header",
            Self::Status => "Set status",
            Self::ReplaceBody => "Replace body text",
            Self::SetQuery => "Set query parameter",
            Self::RemoveQuery => "Remove query parameter",
        }
    }

    /// Placeholders for the first and, if used, second field.
    fn fields(self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::AddHeader | Self::SetHeader => ("Header name", Some("Value")),
            Self::RemoveHeader => ("Header name", None),
            Self::Status => ("Status code", None),
            Self::ReplaceBody => ("Find", Some("Replace with")),
            Self::SetQuery => ("Parameter", Some("Value")),
            Self::RemoveQuery => ("Parameter", None),
        }
    }

    /// Whether the action can choose its side.
    fn sided(self) -> bool {
        matches!(
            self,
            Self::AddHeader | Self::SetHeader | Self::RemoveHeader | Self::ReplaceBody
        )
    }
}

struct ActionRow {
    key: usize,
    kind: ActionKind,
    side: RuleSide,
    first: Entity<InputState>,
    second: Entity<InputState>,
    regex: bool,
}

impl ActionRow {
    fn new(key: usize, action: &RewriteAction, window: &mut Window, cx: &mut App) -> Self {
        let (kind, side, first, second, regex) = match action {
            RewriteAction::AddHeader { side, name, value } => (
                ActionKind::AddHeader,
                *side,
                name.clone(),
                value.clone(),
                false,
            ),
            RewriteAction::SetHeader { side, name, value } => (
                ActionKind::SetHeader,
                *side,
                name.clone(),
                value.clone(),
                false,
            ),
            RewriteAction::RemoveHeader { side, name } => (
                ActionKind::RemoveHeader,
                *side,
                name.clone(),
                String::new(),
                false,
            ),
            RewriteAction::Status { code } => (
                ActionKind::Status,
                RuleSide::Response,
                code.to_string(),
                String::new(),
                false,
            ),
            RewriteAction::ReplaceBody {
                side,
                find,
                replace,
                regex,
            } => (
                ActionKind::ReplaceBody,
                *side,
                find.clone(),
                replace.clone(),
                *regex,
            ),
            RewriteAction::SetQuery { name, value } => (
                ActionKind::SetQuery,
                RuleSide::Request,
                name.clone(),
                value.clone(),
                false,
            ),
            RewriteAction::RemoveQuery { name } => (
                ActionKind::RemoveQuery,
                RuleSide::Request,
                name.clone(),
                String::new(),
                false,
            ),
        };
        Self {
            key,
            kind,
            side,
            first: input(&first, kind.fields().0, window, cx),
            second: input(&second, kind.fields().1.unwrap_or(""), window, cx),
            regex,
        }
    }

    fn action(&self, cx: &App) -> Result<RewriteAction, String> {
        let first = self.first.read(cx).value().to_string();
        let second = self.second.read(cx).value().to_string();
        Ok(match self.kind {
            ActionKind::AddHeader => RewriteAction::AddHeader {
                side: self.side,
                name: first,
                value: second,
            },
            ActionKind::SetHeader => RewriteAction::SetHeader {
                side: self.side,
                name: first,
                value: second,
            },
            ActionKind::RemoveHeader => RewriteAction::RemoveHeader {
                side: self.side,
                name: first,
            },
            ActionKind::Status => RewriteAction::Status {
                code: first
                    .trim()
                    .parse()
                    .map_err(|_| "Enter a status code from 200 to 599")?,
            },
            ActionKind::ReplaceBody => RewriteAction::ReplaceBody {
                side: self.side,
                find: first,
                replace: second,
                regex: self.regex,
            },
            ActionKind::SetQuery => RewriteAction::SetQuery {
                name: first,
                value: second,
            },
            ActionKind::RemoveQuery => RewriteAction::RemoveQuery { name: first },
        })
    }
}

fn input(value: &str, placeholder: &str, window: &mut Window, cx: &mut App) -> Entity<InputState> {
    let placeholder = placeholder.to_owned();
    let value = value.to_owned();
    cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(placeholder)
            .default_value(value)
    })
}

/// The editor for one rule. `id` is `None` until a new rule is saved.
struct Form {
    id: Option<u32>,
    kind: RuleKind,
    enabled: bool,
    name: Entity<InputState>,
    method: Entity<InputState>,
    url: Entity<InputState>,
    app: Entity<InputState>,
    request: bool,
    response: bool,
    path: Entity<InputState>,
    status: Entity<InputState>,
    content_type: Entity<InputState>,
    extra_headers: Entity<TextareaState>,
    scheme: Option<&'static str>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    remote_path: Entity<InputState>,
    keep_query: bool,
    actions: Vec<ActionRow>,
    next_key: usize,
}

impl Form {
    fn new(id: Option<u32>, rule: &Rule, window: &mut Window, cx: &mut App) -> Self {
        let mut form = Self {
            id,
            kind: rule.kind(),
            enabled: rule.enabled,
            name: input(&rule.name, "Rule name", window, cx),
            method: input(rule.method.as_deref().unwrap_or(""), "Any", window, cx),
            url: input(&rule.url, "https://api.example.com/v1/*", window, cx),
            app: input(rule.app.as_deref().unwrap_or(""), "Any app", window, cx),
            request: true,
            response: true,
            path: input("", "/Users/you/mock.json", window, cx),
            status: input("200", "200", window, cx),
            content_type: input("", "From the file extension", window, cx),
            extra_headers: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .rows(3)
                    .placeholder("One header per line, such as X-Mock: yes")
            }),
            scheme: None,
            host: input("", "Keep host", window, cx),
            port: input("", "Keep port", window, cx),
            remote_path: input("", "Keep path", window, cx),
            keep_query: true,
            actions: Vec::new(),
            next_key: 0,
        };
        match &rule.action {
            Action::Breakpoint { request, response } => {
                form.request = *request;
                form.response = *response;
            }
            Action::MapLocal {
                path,
                status,
                content_type,
                headers,
            } => {
                form.path = input(path, "/Users/you/mock.json", window, cx);
                form.status = input(&status.to_string(), "200", window, cx);
                form.content_type = input(
                    content_type.as_deref().unwrap_or(""),
                    "From the file extension",
                    window,
                    cx,
                );
                let text = headers
                    .iter()
                    .map(|(name, value)| format!("{name}: {value}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                form.extra_headers = cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .rows(3)
                        .placeholder("One header per line, such as X-Mock: yes")
                        .default_value(text)
                });
            }
            Action::MapRemote {
                scheme,
                host,
                port,
                path,
                keep_query,
            } => {
                form.scheme = match scheme.as_deref() {
                    Some("http") => Some("http"),
                    Some("https") => Some("https"),
                    _ => None,
                };
                form.host = input(host.as_deref().unwrap_or(""), "Keep host", window, cx);
                form.port = input(
                    &port.map(|port| port.to_string()).unwrap_or_default(),
                    "Keep port",
                    window,
                    cx,
                );
                form.remote_path = input(path.as_deref().unwrap_or(""), "Keep path", window, cx);
                form.keep_query = *keep_query;
            }
            Action::Rewrite { actions } => {
                for action in actions {
                    form.actions
                        .push(ActionRow::new(form.next_key, action, window, cx));
                    form.next_key += 1;
                }
            }
        }
        form
    }

    /// The rule as entered, before validation.
    fn rule(&self, cx: &App) -> Result<Rule, String> {
        let text = |input: &Entity<InputState>| input.read(cx).value().trim().to_owned();
        let optional = |input: &Entity<InputState>| Some(text(input)).filter(|v| !v.is_empty());
        let action = match self.kind {
            RuleKind::Breakpoint => Action::Breakpoint {
                request: self.request,
                response: self.response,
            },
            RuleKind::MapLocal => Action::MapLocal {
                path: text(&self.path),
                status: text(&self.status)
                    .parse()
                    .map_err(|_| "Enter a status code from 200 to 599")?,
                content_type: optional(&self.content_type),
                headers: self
                    .extra_headers
                    .read(cx)
                    .value()
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(|line| {
                        line.split_once(':')
                            .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
                            .ok_or_else(|| format!("Write extra headers as Name: value (“{line}”)"))
                    })
                    .collect::<Result<_, _>>()?,
            },
            RuleKind::MapRemote => Action::MapRemote {
                scheme: self.scheme.map(str::to_owned),
                host: optional(&self.host),
                port: match optional(&self.port) {
                    Some(port) => Some(port.parse().map_err(|_| "Enter a port from 1 to 65535")?),
                    None => None,
                },
                path: optional(&self.remote_path),
                keep_query: self.keep_query,
            },
            RuleKind::Rewrite => Action::Rewrite {
                actions: self
                    .actions
                    .iter()
                    .map(|row| row.action(cx))
                    .collect::<Result<_, _>>()?,
            },
        };
        Ok(Rule {
            id: self.id.unwrap_or(0),
            name: text(&self.name),
            enabled: self.enabled,
            method: optional(&self.method),
            url: text(&self.url),
            app: optional(&self.app),
            action,
        })
    }
}

pub(super) struct RulesWindow {
    form: Option<Form>,
    result: Option<Result<String, String>>,
    _observe: Subscription,
}

impl RulesWindow {
    fn new(cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&global(cx), |_, _, cx| cx.notify());
        Self {
            form: None,
            result: None,
            _observe: observe,
        }
    }

    /// Shows a draft, a saved rule, or the first rule.
    fn show(
        &mut self,
        draft: Option<Rule>,
        select: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rules = rules(cx);
        if let Some(draft) = draft {
            self.form = Some(Form::new(None, &draft, window, cx));
            self.result = Some(Ok("New rule. Save it to apply it.".into()));
        } else if let Some(rule) = select
            .and_then(|id| rules.iter().find(|rule| rule.id == id))
            .or_else(|| self.form.is_none().then(|| rules.first()).flatten())
        {
            self.form = Some(Form::new(Some(rule.id), rule, window, cx));
            self.result = None;
        }
        cx.notify();
    }

    fn select(&mut self, id: u32, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(rule) = rules(cx).into_iter().find(|rule| rule.id == id) {
            self.form = Some(Form::new(Some(id), &rule, window, cx));
            self.result = None;
            cx.notify();
        }
    }

    fn commit(&mut self, rules: Vec<Rule>, success: String, cx: &mut Context<Self>) -> bool {
        let saved = save_rules(rules, cx);
        let ok = saved.is_ok();
        self.result = Some(saved.map(|()| success));
        cx.notify();
        ok
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(form) = &self.form else { return };
        let rule = match form.rule(cx).and_then(Rule::normalize) {
            Ok(rule) => rule,
            Err(error) => {
                self.result = Some(Err(error));
                cx.notify();
                return;
            }
        };
        let mut all = rules(cx);
        let (id, rule) = match form.id {
            Some(id) => (id, Rule { id, ..rule }),
            None => {
                let id = all
                    .iter()
                    .map(|rule| rule.id)
                    .max()
                    .map_or(1, |max| max + 1);
                (id, Rule { id, ..rule })
            }
        };
        match all.iter_mut().find(|existing| existing.id == id) {
            Some(existing) => *existing = rule.clone(),
            None => all.push(rule.clone()),
        }
        if self.commit(all, format!("Saved “{}”", rule.name), cx) {
            if let Some(form) = &mut self.form {
                form.id = Some(id);
            }
        }
    }

    fn set_enabled(&mut self, id: u32, enabled: bool, cx: &mut Context<Self>) {
        let mut all = rules(cx);
        if let Some(rule) = all.iter_mut().find(|rule| rule.id == id) {
            rule.enabled = enabled;
        }
        if self.commit(
            all,
            if enabled {
                "Rule enabled"
            } else {
                "Rule disabled"
            }
            .into(),
            cx,
        ) {
            if let Some(form) = self.form.as_mut().filter(|form| form.id == Some(id)) {
                form.enabled = enabled;
            }
        }
    }

    fn shift(&mut self, id: u32, up: bool, cx: &mut Context<Self>) {
        let mut all = rules(cx);
        let Some(index) = all.iter().position(|rule| rule.id == id) else {
            return;
        };
        let other = if up {
            index.checked_sub(1)
        } else {
            (index + 1 < all.len()).then_some(index + 1)
        };
        if let Some(other) = other {
            all.swap(index, other);
            self.commit(all, "Rule order saved".into(), cx);
        }
    }

    fn delete(&mut self, id: u32, window: &mut Window, cx: &mut Context<Self>) {
        let mut all = rules(cx);
        all.retain(|rule| rule.id != id);
        let next = all.first().map(|rule| rule.id);
        if self.commit(all, "Rule deleted".into(), cx) {
            self.form = None;
            if let Some(next) = next {
                self.select(next, window, cx);
                self.result = Some(Ok("Rule deleted".into()));
            }
        }
    }

    fn rule_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let all = rules(cx);
        let selected = self.form.as_ref().and_then(|form| form.id);
        let muted = cx.theme().muted_foreground;
        v_flex()
            .w(px(300.))
            .h_full()
            .flex_shrink_0()
            .bg(theme::palette(cx).card)
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .p_2()
                    .gap_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("{} rules", all.len())),
                    )
                    .child(
                        Button::new("rules-new")
                            .outline()
                            .small()
                            .icon(IconName::Plus)
                            .label("New")
                            .dropdown_caret(true)
                            .dropdown_menu({
                                let entity = cx.entity();
                                move |mut menu, _, _| {
                                    for kind in RuleKind::ALL {
                                        let entity = entity.clone();
                                        menu = menu.item(PopupMenuItem::new(kind.name()).on_click(
                                            move |_, window, cx| {
                                                entity.update(cx, |this, cx| {
                                                    let draft = template(
                                                        kind,
                                                        "https://api.example.com/*".into(),
                                                        None,
                                                    );
                                                    this.show(Some(draft), None, window, cx)
                                                })
                                            },
                                        ));
                                    }
                                    menu
                                }
                            }),
                    ),
            )
            .child(
                v_flex()
                    .id("rules-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .when(all.is_empty(), |list| {
                        list.child(
                            div().p_3().text_xs().text_color(muted).child(
                                "No rules yet. Choose New, or right-click a flow in Traffic.",
                            ),
                        )
                    })
                    .children(all.iter().enumerate().map(|(index, rule)| {
                        let id = rule.id;
                        let kind = rule.kind();
                        h_flex()
                            .id(("rules-row", index))
                            .h(px(44.))
                            .px_2()
                            .gap_2()
                            .cursor_pointer()
                            .border_b_1()
                            .border_color(cx.theme().table_row_border)
                            .hover(|row| row.bg(cx.theme().table_hover))
                            .when(selected == Some(id), |row| {
                                row.bg(cx.theme().table_active)
                                    .border_l_2()
                                    .border_color(cx.theme().primary)
                            })
                            .on_click(
                                cx.listener(move |this, _, window, cx| this.select(id, window, cx)),
                            )
                            .child(
                                Checkbox::new(("rules-enabled", index))
                                    .small()
                                    .checked(rule.enabled)
                                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                        cx.stop_propagation();
                                        this.set_enabled(id, *checked, cx)
                                    })),
                            )
                            .child(
                                Icon::new(kind_icon(kind))
                                    .size_4()
                                    .flex_shrink_0()
                                    .text_color(kind_color(kind, cx)),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().text_sm().truncate().child(rule.name.clone()))
                                    .child(
                                        div()
                                            .text_xs()
                                            .truncate()
                                            .text_color(muted)
                                            .font_family(cx.theme().mono_font_family.clone())
                                            .child(format!(
                                                "{} {}",
                                                rule.method.as_deref().unwrap_or("ANY"),
                                                rule.url
                                            )),
                                    ),
                            )
                    })),
            )
            .when_some(selected, |panel, id| {
                panel.child(
                    h_flex()
                        .p_2()
                        .gap_1()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .child(
                            Button::new("rules-up")
                                .ghost()
                                .small()
                                .icon(IconName::ArrowUp)
                                .tooltip("Move up (rules apply in list order)")
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.shift(id, true, cx)),
                                ),
                        )
                        .child(
                            Button::new("rules-down")
                                .ghost()
                                .small()
                                .icon(IconName::ArrowDown)
                                .tooltip("Move down")
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.shift(id, false, cx)),
                                ),
                        )
                        .child(div().flex_1())
                        .child(
                            Button::new("rules-delete")
                                .ghost()
                                .small()
                                .icon(IconName::Trash)
                                .label("Delete")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.delete(id, window, cx)
                                })),
                        ),
                )
            })
    }

    fn form_view(&self, form: &Form, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let label = |text: &'static str| {
            div()
                .w(px(110.))
                .flex_shrink_0()
                .text_xs()
                .text_color(muted)
                .child(text)
        };
        let field = |text: &'static str, element: AnyElement| {
            h_flex()
                .gap_2()
                .child(label(text))
                .child(div().flex_1().min_w_0().child(element))
        };
        let kind = form.kind;
        let fields = v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Icon::new(kind_icon(kind))
                            .size_4()
                            .text_color(kind_color(kind, cx)),
                    )
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(kind.name()))
                    .child(div().flex_1())
                    .child(
                        Checkbox::new("rule-enabled")
                            .small()
                            .checked(form.enabled)
                            .label("Enabled")
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                if let Some(form) = &mut this.form {
                                    form.enabled = *checked;
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(field(
                "Name",
                Input::new(&form.name).small().into_any_element(),
            ))
            .child(field(
                "Method",
                Input::new(&form.method).small().into_any_element(),
            ))
            .child(field(
                "URL pattern",
                Input::new(&form.url).small().into_any_element(),
            ))
            .child(div().pl(px(118.)).text_xs().text_color(muted).child(
                "* matches any characters. Without a ?, the query is ignored. \
                     Without ://, any scheme matches.",
            ))
            .child(field(
                "App",
                Input::new(&form.app).small().into_any_element(),
            ));
        let action = match kind {
            RuleKind::Breakpoint => v_flex()
                .gap_2()
                .child(
                    h_flex()
                        .gap_4()
                        .pl(px(118.))
                        .child(
                            Checkbox::new("rule-break-request")
                                .small()
                                .checked(form.request)
                                .label("Hold requests")
                                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                    if let Some(form) = &mut this.form {
                                        form.request = *checked;
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            Checkbox::new("rule-break-response")
                                .small()
                                .checked(form.response)
                                .label("Hold responses")
                                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                    if let Some(form) = &mut this.form {
                                        form.response = *checked;
                                    }
                                    cx.notify();
                                })),
                        ),
                )
                .child(div().pl(px(118.)).text_xs().text_color(muted).child(
                    "Held messages open in Breakpoints (⇧⌘B). They continue unmodified after \
                     2 minutes. ⌘B turns every breakpoint off or on.",
                )),
            RuleKind::MapLocal => v_flex()
                .gap_2()
                .child(
                    h_flex()
                        .gap_2()
                        .child(label("File"))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(Input::new(&form.path).small()),
                        )
                        .child(
                            Button::new("rule-choose-file")
                                .outline()
                                .small()
                                .label("Choose…")
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.choose_file(window, cx)),
                                ),
                        ),
                )
                .child(field(
                    "Status",
                    Input::new(&form.status).small().into_any_element(),
                ))
                .child(field(
                    "Content type",
                    Input::new(&form.content_type).small().into_any_element(),
                ))
                .child(field(
                    "Extra headers",
                    Textarea::new(&form.extra_headers).into_any_element(),
                ))
                .child(div().pl(px(118.)).text_xs().text_color(muted).child(
                    "Regular files up to 10 MB, read when a request matches. \
                     The origin is not contacted.",
                )),
            RuleKind::MapRemote => {
                let scheme = form.scheme;
                let entity = cx.entity();
                v_flex()
                    .gap_2()
                    .child(field(
                        "Scheme",
                        Button::new("rule-remote-scheme")
                            .outline()
                            .small()
                            .label(scheme.unwrap_or("Keep scheme"))
                            .dropdown_caret(true)
                            .dropdown_menu(move |mut menu, _, _| {
                                for option in [None, Some("http"), Some("https")] {
                                    let entity = entity.clone();
                                    menu = menu.item(
                                        PopupMenuItem::new(option.unwrap_or("Keep scheme"))
                                            .checked(scheme == option)
                                            .on_click(move |_, _, cx| {
                                                entity.update(cx, |this, cx| {
                                                    if let Some(form) = &mut this.form {
                                                        form.scheme = option;
                                                    }
                                                    cx.notify();
                                                })
                                            }),
                                    );
                                }
                                menu
                            })
                            .into_any_element(),
                    ))
                    .child(field(
                        "Host",
                        Input::new(&form.host).small().into_any_element(),
                    ))
                    .child(field(
                        "Port",
                        Input::new(&form.port).small().into_any_element(),
                    ))
                    .child(field(
                        "Path",
                        Input::new(&form.remote_path).small().into_any_element(),
                    ))
                    .child(
                        div().pl(px(118.)).child(
                            Checkbox::new("rule-keep-query")
                                .small()
                                .checked(form.keep_query)
                                .label("Keep the query string")
                                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                    if let Some(form) = &mut this.form {
                                        form.keep_query = *checked;
                                    }
                                    cx.notify();
                                })),
                        ),
                    )
                    .child(div().pl(px(118.)).text_xs().text_color(muted).child(
                        "The new destination follows the destination policy. HTTPS is \
                         verified with the macOS trust store.",
                    ))
            }
            RuleKind::Rewrite => self.actions_view(form, cx),
        };
        v_flex()
            .gap_3()
            .child(fields)
            .child(div().h(px(1.)).bg(cx.theme().border))
            .child(action)
    }

    fn actions_view(&self, form: &Form, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .child(
                        div()
                            .flex_1()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Actions"),
                    )
                    .child(
                        Button::new("rule-add-action")
                            .outline()
                            .small()
                            .icon(IconName::Plus)
                            .label("Add action")
                            .on_click(cx.listener(|this, _, window, cx| {
                                if let Some(form) = &mut this.form {
                                    let row = ActionRow::new(
                                        form.next_key,
                                        &RewriteAction::SetHeader {
                                            side: RuleSide::Response,
                                            name: String::new(),
                                            value: String::new(),
                                        },
                                        window,
                                        cx,
                                    );
                                    form.next_key += 1;
                                    form.actions.push(row);
                                }
                                cx.notify();
                            })),
                    ),
            )
            .children(form.actions.iter().map(|row| {
                let key = row.key;
                let entity = cx.entity();
                let second = row.kind.fields().1;
                let kind = row.kind;
                let side = row.side;
                let kinds = {
                    let entity = entity.clone();
                    move |mut menu: gpui_kit::component::menu::PopupMenu,
                          _: &mut Window,
                          _: &mut Context<gpui_kit::component::menu::PopupMenu>| {
                        for option in ActionKind::ALL {
                            let entity = entity.clone();
                            menu = menu.item(
                                PopupMenuItem::new(option.name())
                                    .checked(option == kind)
                                    .on_click(move |_, window, cx| {
                                        entity.update(cx, |this, cx| {
                                            this.set_action_kind(key, option, window, cx)
                                        })
                                    }),
                            );
                        }
                        menu
                    }
                };
                let sides = {
                    let entity = entity.clone();
                    move |mut menu: gpui_kit::component::menu::PopupMenu,
                          _: &mut Window,
                          _: &mut Context<gpui_kit::component::menu::PopupMenu>| {
                        for option in [RuleSide::Request, RuleSide::Response] {
                            let entity = entity.clone();
                            menu = menu.item(
                                PopupMenuItem::new(side_name(option))
                                    .checked(option == side)
                                    .on_click(move |_, _, cx| {
                                        entity.update(cx, |this, cx| {
                                            if let Some(row) = this.row(key) {
                                                row.side = option;
                                            }
                                            cx.notify();
                                        })
                                    }),
                            );
                        }
                        menu
                    }
                };
                v_flex()
                    .id(("rule-action", key))
                    .p_2()
                    .gap_1p5()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new(("rule-action-kind", key))
                                    .outline()
                                    .small()
                                    .label(kind.name())
                                    .dropdown_caret(true)
                                    .dropdown_menu(kinds),
                            )
                            .child(if kind.sided() {
                                Button::new(("rule-action-side", key))
                                    .outline()
                                    .small()
                                    .label(side_name(side))
                                    .dropdown_caret(true)
                                    .dropdown_menu(sides)
                                    .into_any_element()
                            } else {
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(side_name(if kind == ActionKind::Status {
                                        RuleSide::Response
                                    } else {
                                        RuleSide::Request
                                    }))
                                    .into_any_element()
                            })
                            .when(kind == ActionKind::ReplaceBody, |row_view| {
                                row_view.child(
                                    Checkbox::new(("rule-action-regex", key))
                                        .small()
                                        .checked(row.regex)
                                        .label("Regex")
                                        .on_click(cx.listener(
                                            move |this, checked: &bool, _, cx| {
                                                if let Some(row) = this.row(key) {
                                                    row.regex = *checked;
                                                }
                                                cx.notify();
                                            },
                                        )),
                                )
                            })
                            .child(div().flex_1())
                            .child(
                                Button::new(("rule-action-remove", key))
                                    .ghost()
                                    .small()
                                    .label("Remove")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(form) = &mut this.form {
                                            form.actions.retain(|row| row.key != key);
                                        }
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(Input::new(&row.first).small()),
                            )
                            .when(second.is_some(), |fields| {
                                fields.child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .child(Input::new(&row.second).small()),
                                )
                            }),
                    )
                    .when(kind == ActionKind::ReplaceBody, |view| {
                        view.child(div().text_xs().text_color(muted).child(
                            "Text bodies up to 1 MB. Compressed bodies are decoded and sent \
                             uncompressed. Regex replacements can use $1.",
                        ))
                    })
            }))
    }

    fn row(&mut self, key: usize) -> Option<&mut ActionRow> {
        self.form
            .as_mut()?
            .actions
            .iter_mut()
            .find(|row| row.key == key)
    }

    fn set_action_kind(
        &mut self,
        key: usize,
        kind: ActionKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(row) = self.row(key) {
            row.kind = kind;
            let (first, second) = kind.fields();
            let (first_entity, second_entity) = (row.first.clone(), row.second.clone());
            first_entity.update(cx, |input, cx| input.set_placeholder(first, window, cx));
            second_entity.update(cx, |input, cx| {
                input.set_placeholder(second.unwrap_or(""), window, cx)
            });
        }
        cx.notify();
    }

    fn choose_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose a file to serve".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                if let Some(path) = paths.into_iter().next() {
                    let _ = this.update_in(cx, |this, window, cx| {
                        if let Some(form) = &this.form {
                            let text = path.to_string_lossy().into_owned();
                            form.path
                                .update(cx, |input, cx| input.set_value(text, window, cx));
                        }
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }
}

fn side_name(side: RuleSide) -> &'static str {
    match side {
        RuleSide::Request => "Request",
        RuleSide::Response => "Response",
    }
}

impl Render for RulesWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let editor = match self.form.take() {
            Some(form) => {
                let view = self.form_view(&form, cx).into_any_element();
                self.form = Some(form);
                view
            }
            None => v_flex()
                .size_full()
                .justify_center()
                .items_center()
                .gap_2()
                .child(
                    Icon::new(IconName::ListChecks)
                        .size_6()
                        .text_color(cx.theme().muted_foreground),
                )
                .child(div().child("Choose New to create a rule"))
                .into_any_element(),
        };
        let saved = self.form.as_ref().is_some_and(|form| form.id.is_some());
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
                            .child(div().text_sm().child("Rules"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Breakpoints, Map Local, Map Remote and rewrites"),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .child(self.rule_list(cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .id("rules-form")
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
                                        Button::new("rules-close")
                                            .outline()
                                            .label("Close")
                                            .on_click(|_, window, _| {
                                                if !close_natively(window) {
                                                    window.remove_window();
                                                }
                                            }),
                                    )
                                    .child(
                                        Button::new("rules-save")
                                            .primary()
                                            .disabled(self.form.is_none())
                                            .label(if saved { "Save" } else { "Save rule" })
                                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                                    ),
                            ),
                    ),
            )
    }
}
