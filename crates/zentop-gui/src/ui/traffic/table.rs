//! The dense flow table: sortable columns with a visibility menu, 24 px
//! virtual rows and the row context menu.
use std::cmp::Ordering;

use super::navigation::{domain, hostname, AppKey, Scope};
use super::*;
use gpui_kit::component::menu::ContextMenuExt;
use gpui_kit::component::menu::PopupMenu;
use gpui_kit::{Hsla, MouseButton, ScrollStrategy};

pub(super) const ROW_HEIGHT: f32 = 24.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Column {
    Dot,
    Id,
    Url,
    Client,
    Method,
    Status,
    Code,
    Time,
    Duration,
    RequestSize,
    ResponseSize,
}

impl Column {
    pub(super) const ALL: [Self; 11] = [
        Self::Dot,
        Self::Id,
        Self::Url,
        Self::Client,
        Self::Method,
        Self::Status,
        Self::Code,
        Self::Time,
        Self::Duration,
        Self::RequestSize,
        Self::ResponseSize,
    ];

    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Dot => "",
            Self::Id => "ID",
            Self::Url => "URL",
            Self::Client => "Client",
            Self::Method => "Method",
            Self::Status => "Status",
            Self::Code => "Code",
            Self::Time => "Time",
            Self::Duration => "Duration",
            Self::RequestSize => "Request",
            Self::ResponseSize => "Response",
        }
    }

    fn menu_name(self) -> &'static str {
        match self {
            Self::Dot => "Status dot",
            Self::RequestSize => "Request size",
            Self::ResponseSize => "Response size",
            column => column.title(),
        }
    }

    /// Fixed widths; the URL takes the remaining space.
    pub(super) fn width(self) -> f32 {
        match self {
            Self::Dot => 10.,
            Self::Id => 40.,
            Self::Url => 0.,
            Self::Client => 118.,
            Self::Method => 54.,
            Self::Status => 70.,
            Self::Code => 36.,
            Self::Time => 86.,
            Self::Duration => 62.,
            Self::RequestSize => 62.,
            Self::ResponseSize => 62.,
        }
    }

    fn right_aligned(self) -> bool {
        matches!(
            self,
            Self::Id | Self::Duration | Self::RequestSize | Self::ResponseSize
        )
    }

    fn compare(self, a: &Metadata, b: &Metadata) -> Ordering {
        match self {
            Self::Dot | Self::Status => status_label(a).cmp(status_label(b)),
            Self::Id => a.id.cmp(&b.id),
            Self::Url => a.url.cmp(&b.url),
            Self::Client => a.process.name().cmp(b.process.name()),
            Self::Method => a.method.cmp(&b.method),
            Self::Code => a.status.cmp(&b.status),
            Self::Time => a.started.cmp(&b.started),
            Self::Duration => a.timing.total.cmp(&b.timing.total),
            Self::RequestSize => a.bytes.0.cmp(&b.bytes.0),
            Self::ResponseSize => a.bytes.1.cmp(&b.bytes.1),
        }
    }
}

pub(super) struct Sort {
    pub(super) column: Column,
    pub(super) descending: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Self {
            column: Column::Id,
            descending: true,
        }
    }
}

impl Sort {
    pub(super) fn select(&mut self, column: Column) {
        self.descending = if self.column == column {
            !self.descending
        } else {
            false
        };
        self.column = column;
    }

    pub(super) fn order(&self, a: &Metadata, b: &Metadata) -> Ordering {
        let order = self.column.compare(a, b);
        let order = if self.descending {
            order.reverse()
        } else {
            order
        };
        order.then_with(|| b.id.cmp(&a.id))
    }
}

/// Which columns are shown. The URL always is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Columns([bool; Column::ALL.len()]);

impl Default for Columns {
    fn default() -> Self {
        Self([true; Column::ALL.len()])
    }
}

impl Columns {
    fn index(column: Column) -> usize {
        Column::ALL.iter().position(|c| *c == column).unwrap()
    }

    pub(super) fn shows(&self, column: Column) -> bool {
        self.0[Self::index(column)]
    }

    pub(super) fn toggle(&mut self, column: Column) -> bool {
        if column == Column::Url {
            return false;
        }
        let shown = &mut self.0[Self::index(column)];
        *shown = !*shown;
        true
    }

    pub(super) fn visible(self) -> impl Iterator<Item = Column> {
        Column::ALL
            .into_iter()
            .filter(move |column| self.shows(*column))
    }
}

pub(super) fn status_label(row: &Metadata) -> &'static str {
    match &row.state {
        FlowState::Refused(_) => "Refused",
        FlowState::Failed(_) | FlowState::UpstreamTls(_) => "Failed",
        _ if row.kind == FlowKind::Tunnel => "Tunnel",
        FlowState::Active => "Active",
        FlowState::Complete => "Completed",
    }
}

fn dot_color(row: &Metadata, cx: &App) -> Hsla {
    match status_label(row) {
        "Completed" => theme::palette(cx).running,
        "Active" => theme::palette(cx).warning,
        "Failed" => theme::palette(cx).danger,
        _ => cx.theme().muted_foreground,
    }
}

pub(super) fn code_color(status: Option<u16>, cx: &App) -> Hsla {
    match status {
        Some(500..=599) => theme::palette(cx).danger,
        Some(400..=499) => theme::palette(cx).warning,
        _ => cx.theme().foreground,
    }
}

pub(super) fn duration_text(duration: Duration) -> String {
    let ms = duration.as_secs_f64() * 1_000.;
    if ms < 1_000. {
        format!("{ms:.0} ms")
    } else {
        format!("{:.2} s", ms / 1_000.)
    }
}

pub(super) fn time_text(time: std::time::SystemTime) -> String {
    chrono::DateTime::<chrono::Local>::from(time)
        .format("%H:%M:%S%.3f")
        .to_string()
}

fn cell(column: Column, content: impl IntoElement) -> Div {
    let cell = div().flex_shrink_0().overflow_hidden().whitespace_nowrap();
    let cell = if column == Column::Url {
        cell.flex_1().min_w_0()
    } else {
        cell.w(px(column.width()))
    };
    cell.when(column.right_aligned(), |cell| cell.text_right())
        .child(content)
}

fn shortcut(label: &'static str, keys: &'static str) -> PopupMenuItem {
    PopupMenuItem::element(move |_, cx| {
        h_flex()
            .w_full()
            .gap_6()
            .child(label)
            .child(div().flex_1())
            .child(div().text_color(cx.theme().muted_foreground).child(keys))
    })
}

fn columns_menu(mut menu: PopupMenu, entity: &Entity<Traffic>, columns: Columns) -> PopupMenu {
    for column in Column::ALL {
        let entity = entity.clone();
        menu = menu.item(
            PopupMenuItem::new(column.menu_name())
                .checked(columns.shows(column))
                .disabled(column == Column::Url)
                .on_click(move |_, _, cx| {
                    entity.update(cx, |this, cx| {
                        this.columns.toggle(column);
                        cx.notify();
                    })
                }),
        );
    }
    menu
}

impl Traffic {
    pub(super) fn table(&self, matches: Arc<[usize]>, cx: &Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let empty = matches.is_empty();
        let columns = self.columns;
        let header_entity = entity.clone();
        let header = h_flex()
            .id("traffic-columns")
            .w_full()
            .h(px(ROW_HEIGHT))
            .flex_shrink_0()
            .gap_2()
            .px_2()
            .text_xs()
            .bg(cx.theme().table_head)
            .text_color(cx.theme().table_head_foreground)
            .border_b_1()
            .border_color(cx.theme().border)
            .children(columns.visible().map(|column| {
                let mut title = column.title().to_owned();
                if self.sort.column == column {
                    title.push_str(if self.sort.descending { " ↓" } else { " ↑" });
                }
                cell(column, title)
                    .id(column.menu_name())
                    .h_full()
                    .flex()
                    .items_center()
                    .when(column.right_aligned(), |cell| cell.justify_end())
                    .cursor_pointer()
                    .hover(|cell| cell.text_color(cx.theme().foreground))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.sort.select(column);
                        this.focus.focus(window, cx);
                        cx.notify();
                    }))
            }))
            .child(
                Button::new("traffic-column-menu")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Columns3)
                    .tooltip("Columns")
                    .dropdown_menu(move |menu, _, cx| {
                        let columns = header_entity.read(cx).columns;
                        columns_menu(menu, &header_entity, columns)
                    }),
            )
            .context_menu({
                let entity = entity.clone();
                move |menu, _, cx| {
                    let columns = entity.read(cx).columns;
                    columns_menu(menu, &entity, columns)
                }
            });
        v_flex()
            .id("traffic-table")
            .track_focus(&self.focus)
            .size_full()
            .min_h_0()
            .overflow_hidden()
            .child(header)
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
                                .child(if self.capture.read(cx).session.is_some() {
                                    "Route a program through the proxy, or reset the filters."
                                } else {
                                    "Start capture from the status pill above."
                                }),
                        ),
                )
            })
            .when(!empty, |panel| {
                panel.child(
                    uniform_list("traffic-flows", matches.len(), move |range, _, cx| {
                        let this = entity.read(cx);
                        range
                            .filter_map(|position| {
                                let row = this.rows.get(*matches.get(position)?)?;
                                Some(flow_row(row, position, this, &entity, cx))
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

    /// Selects a row the way a click does, keeping the detail pane's place.
    pub(super) fn select_row(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        if self.selected != Some(id) {
            self.select(Some(id));
            self.refresh_detail(cx);
            // Opening the detail pane shrinks the table; keep the row in view.
            cx.on_next_frame(window, move |this, _, cx| this.scroll_to(id, cx));
        }
        cx.notify();
    }

    pub(super) fn scroll_to(&self, id: u64, cx: &App) {
        if let Some(position) = self
            .matching(cx)
            .iter()
            .position(|&index| self.rows[index].id == id)
        {
            self.list_scroll
                .scroll_to_item(position, ScrollStrategy::Nearest);
        }
    }
}

fn flow_row(
    row: &Metadata,
    position: usize,
    this: &Traffic,
    entity: &Entity<Traffic>,
    cx: &App,
) -> AnyElement {
    let id = row.id;
    let selected = this.selected == Some(id);
    let muted = cx.theme().muted_foreground;
    let tunnel = row.kind == FlowKind::Tunnel;
    let icon = identity(&row.process)
        .and_then(|owner| this.icons.get(&owner))
        .and_then(Clone::clone);
    let select = entity.clone();
    let right_select = entity.clone();
    let menu_entity = entity.clone();
    let menu_row = row.clone();
    h_flex()
        .id(("traffic-flow", id as usize))
        .w_full()
        .h(px(ROW_HEIGHT))
        .gap_2()
        .px_2()
        .text_xs()
        .cursor_pointer()
        .when(position % 2 == 1, |row| {
            row.bg(cx.theme().muted.opacity(0.35))
        })
        .hover(|row| row.bg(cx.theme().table_hover))
        .when(selected, |row| {
            row.bg(cx.theme().table_active)
                .border_l_2()
                .border_color(cx.theme().primary)
        })
        .when(tunnel, |row| row.text_color(muted))
        .on_click(move |_, window, cx| {
            select.update(cx, |this, cx| this.select_row(id, window, cx))
        })
        .on_mouse_down(MouseButton::Right, move |_, window, cx| {
            right_select.update(cx, |this, cx| this.select_row(id, window, cx))
        })
        .children(this.columns.visible().map(|column| {
            let content = match column {
                Column::Dot => div()
                    .size(px(7.))
                    .rounded_full()
                    .bg(dot_color(row, cx))
                    .into_any_element(),
                Column::Id => div()
                    .text_color(muted)
                    .child(id.to_string())
                    .into_any_element(),
                Column::Url => h_flex()
                    .gap_1()
                    .min_w_0()
                    .when(row.repeated, |cell| {
                        cell.child(
                            Icon::new(IconName::Repeat)
                                .size_3()
                                .flex_shrink_0()
                                .text_color(muted),
                        )
                    })
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(cx.theme().mono_font_family.clone())
                            .child(row.url.clone()),
                    )
                    .into_any_element(),
                Column::Client => h_flex()
                    .gap_1()
                    .min_w_0()
                    .child(match icon.clone() {
                        Some(image) => gpui_kit::img(image)
                            .size(px(14.))
                            .flex_shrink_0()
                            .into_any_element(),
                        None => Icon::new(IconName::AppWindow)
                            .size(px(14.))
                            .flex_shrink_0()
                            .text_color(muted)
                            .into_any_element(),
                    })
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .child(row.process.name().to_owned()),
                    )
                    .into_any_element(),
                Column::Method => div()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .child(row.method.clone())
                    .into_any_element(),
                Column::Status => div().child(status_label(row)).into_any_element(),
                Column::Code => div()
                    .text_color(code_color(row.status, cx))
                    .child(row.status.map_or_else(String::new, |code| code.to_string()))
                    .into_any_element(),
                Column::Time => div().child(time_text(row.started)).into_any_element(),
                Column::Duration => div()
                    .child(if row.state == FlowState::Active {
                        "—".to_owned()
                    } else {
                        duration_text(row.timing.total)
                    })
                    .into_any_element(),
                Column::RequestSize => div().child(format_bytes(row.bytes.0)).into_any_element(),
                Column::ResponseSize => div().child(format_bytes(row.bytes.1)).into_any_element(),
            };
            cell(column, content)
                .flex()
                .items_center()
                .when(column.right_aligned(), |cell| cell.justify_end())
        }))
        // Room for the header's column menu button.
        .child(div().w(px(20.)).flex_shrink_0())
        .context_menu(move |menu, _, cx| row_menu(menu, &menu_entity, &menu_row, cx))
        .into_any_element()
}

fn row_menu(menu: PopupMenu, entity: &Entity<Traffic>, row: &Metadata, cx: &App) -> PopupMenu {
    let this = entity.read(cx);
    let id = row.id;
    let replayable = this
        .capture
        .read(cx)
        .session
        .as_ref()
        .and_then(|session| session.flow(id, false))
        .is_some_and(|flow| flow.replayable)
        && !this.counters.paused;
    let http = matches!(row.kind, FlowKind::Http | FlowKind::DecryptedHttps);
    let app = AppKey::from_row(row);
    let app_scope = Scope::App(app.clone());
    let domain_name = domain(hostname(&row.host)).to_owned();
    let domain_scope = Scope::Domain(domain_name.clone());
    let decrypt = (row.kind == FlowKind::Tunnel)
        .then(|| zentop_proxy::decrypt_host(hostname(&row.host)).ok())
        .flatten()
        .filter(|host| !certificate::hosts(cx).contains(host));
    let url = row.url.clone();
    let on = |action: fn(&mut Traffic, u64, &mut Context<Traffic>)| {
        let entity = entity.clone();
        move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
            entity.update(cx, |this, cx| action(this, id, cx))
        }
    };
    let pin = |scope: Scope| {
        let entity = entity.clone();
        move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
            entity.update(cx, |this, cx| this.toggle_pin(&scope, cx))
        }
    };
    let pinned_app = this.pins.is_pinned(&app_scope);
    let pinned_domain = this.pins.is_pinned(&domain_scope);
    let decrypt_entity = entity.clone();
    menu.item(
        PopupMenuItem::new("Copy URL").on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(url.clone()))
        }),
    )
    .item(
        shortcut("Copy as cURL", "⇧⌘C")
            .disabled(!http)
            .on_click(on(|this, id, cx| this.copy_curl(id, cx))),
    )
    .separator()
    .item(
        shortcut("Repeat", "⌘R")
            .disabled(!replayable)
            .on_click(on(|this, id, cx| this.repeat(id, cx))),
    )
    .item(
        shortcut("Edit and resend", "⌘E")
            .disabled(!replayable)
            .on_click(on(|this, id, cx| this.open_editor(Some(id), cx))),
    )
    .separator()
    .item(
        PopupMenuItem::new(if pinned_app {
            format!("Unpin app “{}”", app.name)
        } else {
            format!("Pin app “{}”", app.name)
        })
        .disabled(!app.known)
        .on_click(pin(app_scope)),
    )
    .item(
        PopupMenuItem::new(if pinned_domain {
            format!("Unpin domain “{domain_name}”")
        } else {
            format!("Pin domain “{domain_name}”")
        })
        .on_click(pin(domain_scope)),
    )
    .item(
        PopupMenuItem::new("Decrypt this host")
            .disabled(decrypt.is_none())
            .on_click(move |_, _, cx| {
                if let Some(host) = &decrypt {
                    decrypt_entity.update(cx, |this, cx| this.edit_host(host, true, cx));
                }
            }),
    )
    .separator()
    .item(
        PopupMenuItem::new("Export HAR…")
            .on_click(on(|this, id, cx| this.export_har(vec![id], cx))),
    )
}

#[cfg(test)]
mod tests {
    use super::super::navigation::tests::{known, row};
    use super::*;

    #[test]
    fn sorts_each_column_with_stable_id_ties() {
        let mut a = row(1, "z:80");
        let mut b = row(2, "a:80");
        let mut sort = Sort::default();
        // Newest first by default.
        assert_eq!(sort.order(&a, &b), Ordering::Greater);
        sort.select(Column::Url);
        assert_eq!(sort.order(&a, &b), Ordering::Greater);
        sort.select(Column::Url);
        assert_eq!(sort.order(&a, &b), Ordering::Less);
        sort.select(Column::ResponseSize);
        b.bytes.1 = 20;
        assert_eq!(sort.order(&a, &b), Ordering::Less);
        sort.select(Column::RequestSize);
        a.bytes.0 = 9;
        assert_eq!(sort.order(&a, &b), Ordering::Greater);
        sort.select(Column::Code);
        b.status = Some(5);
        assert_eq!(sort.order(&a, &b), Ordering::Greater);
        sort.select(Column::Status);
        b.state = FlowState::Failed("x");
        assert_eq!(sort.order(&a, &b), Ordering::Less);
        sort.select(Column::Client);
        let a = known(a, "zsh", 1);
        assert_eq!(sort.order(&a, &b), Ordering::Greater);
        // Equal values fall back to newest first.
        sort.select(Column::Method);
        assert_eq!(sort.order(&a, &b), Ordering::Greater);
    }

    #[test]
    fn column_visibility_keeps_the_url() {
        let mut columns = Columns::default();
        assert_eq!(columns.visible().count(), Column::ALL.len());
        assert!(!columns.toggle(Column::Url));
        assert!(columns.toggle(Column::Id));
        assert!(columns.toggle(Column::Time));
        assert!(!columns.shows(Column::Id));
        assert_eq!(columns.visible().count(), Column::ALL.len() - 2);
        assert!(columns.visible().any(|column| column == Column::Url));
        columns.toggle(Column::Id);
        assert!(columns.shows(Column::Id));
    }

    #[test]
    fn status_labels_and_durations() {
        let mut flow = row(1, "x:443");
        assert_eq!(status_label(&flow), "Completed");
        flow.kind = FlowKind::Tunnel;
        assert_eq!(status_label(&flow), "Tunnel");
        flow.state = FlowState::Refused("policy");
        assert_eq!(status_label(&flow), "Refused");
        flow.state = FlowState::UpstreamTls("bad".into());
        assert_eq!(status_label(&flow), "Failed");
        flow.kind = FlowKind::Http;
        flow.state = FlowState::Active;
        assert_eq!(status_label(&flow), "Active");
        assert_eq!(duration_text(Duration::from_micros(255_400)), "255 ms");
        assert_eq!(duration_text(Duration::from_millis(1_234)), "1.23 s");
    }
}
