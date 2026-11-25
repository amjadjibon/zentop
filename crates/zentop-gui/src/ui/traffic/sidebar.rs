//! Renders the Favorites and All tree with a filter field at the bottom.
use super::navigation::{tree, RowIcon, TreeRow};
use super::*;
use gpui_kit::component::menu::ContextMenuExt;
use gpui_kit::{FontWeight, ScrollStrategy};

const ROW_HEIGHT: f32 = 24.;

impl Traffic {
    pub(super) fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let filter = self.sidebar_filter.read(cx).value().to_string();
        let rows: Arc<[TreeRow]> = tree(&self.sidebar, &self.pins, &self.expanded, &filter).into();
        let entity = cx.entity();
        v_flex()
            .w(px(214.))
            .h_full()
            .min_h_0()
            .flex_shrink_0()
            .text_xs()
            .bg(theme::palette(cx).card)
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                uniform_list("traffic-sidebar", rows.len(), move |range, _, cx| {
                    range
                        .filter_map(|index| Some(tree_row(index, rows.get(index)?, &entity, cx)))
                        .collect::<Vec<_>>()
                })
                .flex_1()
                .min_h_0()
                .w_full()
                .py_1(),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .p_1p5()
                    .gap_1()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        div().flex_1().child(
                            Input::new(&self.sidebar_filter)
                                .small()
                                .prefix(Icon::new(IconName::Search).size_3())
                                .suffix(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("⇧⌘F"),
                                ),
                        ),
                    ),
            )
    }

    fn toggle_node(&mut self, node: &Node, cx: &mut Context<Self>) {
        if !self.expanded.remove(node) {
            self.expanded.insert(node.clone());
        }
        cx.notify();
    }
}

fn tree_row(index: usize, row: &TreeRow, entity: &Entity<Traffic>, cx: &App) -> AnyElement {
    let this = entity.read(cx);
    let selected = row.scope.is_some() && this.scope == row.scope;
    let muted = cx.theme().muted_foreground;
    let icon = match row.icon {
        RowIcon::None => None,
        RowIcon::App => Some(
            match row
                .identity
                .and_then(|id| this.icons.get(&id))
                .and_then(Clone::clone)
            {
                Some(image) => gpui_kit::img(image).size(px(14.)).into_any_element(),
                None => Icon::new(IconName::AppWindow)
                    .size(px(14.))
                    .text_color(muted)
                    .into_any_element(),
            },
        ),
        RowIcon::Domain => Some(
            Icon::new(IconName::Globe)
                .size(px(13.))
                .text_color(muted)
                .into_any_element(),
        ),
        RowIcon::Host => Some(
            Icon::new(IconName::Server)
                .size(px(13.))
                .text_color(muted)
                .into_any_element(),
        ),
        RowIcon::Pin => Some(
            Icon::new(IconName::Pin)
                .size(px(13.))
                .text_color(muted)
                .into_any_element(),
        ),
    };
    let chevron = row.node.as_ref().map(|(node, open)| {
        let node = node.clone();
        let toggle = entity.clone();
        div()
            .id(("traffic-tree-chevron", index))
            .w(px(12.))
            .flex_shrink_0()
            .text_color(muted)
            .cursor_pointer()
            .child(if *open { "▾" } else { "▸" })
            .on_click(move |_, _, cx| {
                cx.stop_propagation();
                toggle.update(cx, |this, cx| this.toggle_node(&node, cx))
            })
    });
    let pinnable = row.scope.as_ref().filter(|scope| match scope {
        Scope::App(app) => app.known,
        Scope::Domain(_) => true,
        _ => false,
    });
    let pinned = pinnable.is_some_and(|scope| this.pins.is_pinned(scope));
    let click = entity.clone();
    let scope = row.scope.clone();
    let node = row.node.as_ref().map(|(node, _)| node.clone());
    let element = h_flex()
        .id(("traffic-tree-row", index))
        .group("traffic-tree-row")
        .h(px(ROW_HEIGHT))
        .w_full()
        .pr_2()
        .pl(px(6. + f32::from(row.depth) * 12.))
        .gap_1()
        .cursor_pointer()
        .hover(|row| row.bg(cx.theme().table_hover))
        .when(selected, |row| row.bg(cx.theme().table_active))
        .when(row.section, |section| {
            section
                .font_weight(FontWeight::SEMIBOLD)
                .when(row.depth == 0, |section| section.text_color(muted))
        })
        .when(row.section && row.node.is_none(), |section| {
            section.text_color(muted)
        })
        .child(chevron.map_or_else(
            || div().w(px(12.)).flex_shrink_0().into_any_element(),
            IntoElement::into_any_element,
        ))
        .when_some(icon, |row, icon| {
            row.child(div().flex_shrink_0().child(icon))
        })
        .child(div().flex_1().min_w_0().truncate().child(row.label.clone()))
        .when_some(pinnable.cloned(), |element, scope| {
            let pin = entity.clone();
            element.child(
                div()
                    .id(("traffic-tree-pin", index))
                    .flex_shrink_0()
                    .invisible()
                    .group_hover("traffic-tree-row", |s| s.visible())
                    .child(
                        Icon::new(if pinned {
                            IconName::PinOff
                        } else {
                            IconName::Pin
                        })
                        .size(px(12.))
                        .text_color(muted),
                    )
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        pin.update(cx, |this, cx| this.toggle_pin(&scope, cx))
                    }),
            )
        })
        .when_some(row.count, |element, count| {
            element.child(
                div()
                    .flex_shrink_0()
                    .text_color(muted)
                    .child(count.to_string()),
            )
        })
        .on_click(move |_, window, cx| {
            click.update(cx, |this, cx| {
                if let Some(scope) = &scope {
                    this.scope = (this.scope.as_ref() != Some(scope)).then(|| scope.clone());
                    this.list_scroll.scroll_to_item(0, ScrollStrategy::Top);
                    this.focus.focus(window, cx);
                    cx.notify();
                } else if let Some(node) = &node {
                    this.toggle_node(node, cx);
                }
            })
        });
    match pinnable.cloned() {
        Some(scope) => {
            let menu = entity.clone();
            element
                .context_menu(move |menu_builder, _, cx| {
                    let pinned = menu.read(cx).pins.is_pinned(&scope);
                    let toggle = menu.clone();
                    let scope = scope.clone();
                    menu_builder.item(
                        PopupMenuItem::new(if pinned { "Unpin" } else { "Pin to Favorites" })
                            .on_click(move |_, _, cx| {
                                toggle.update(cx, |this, cx| this.toggle_pin(&scope, cx))
                            }),
                    )
                })
                .into_any_element()
        }
        None => element.into_any_element(),
    }
}
