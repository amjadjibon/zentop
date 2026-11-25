//! The title-bar toolbar, the content-type tab strip, the filter bar and the
//! bottom status bar.
use super::filter::Transport;
use super::setup::CaptureState;
use super::*;
use zentop_proxy::ContentKind;

const MEMORY_BUDGET: u64 = 64 * 1024 * 1024;

fn icon_button(id: &'static str, icon: IconName, tooltip: &'static str) -> Button {
    Button::new(id).ghost().small().icon(icon).tooltip(tooltip)
}

fn divider(cx: &App) -> Div {
    div()
        .mx_1()
        .w(px(1.))
        .h(px(14.))
        .flex_shrink_0()
        .bg(cx.theme().border)
}

impl Traffic {
    pub(super) fn title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.capture_state(cx);
        let running = state != CaptureState::Stopped;
        let placement = self.placement;
        let pill = self.status_pill(cx);
        let open_app = self.open_app_menu(cx);
        let breakpoints = breakpoints_enabled(cx);
        let held = self
            .capture
            .read(cx)
            .session
            .as_ref()
            .map_or(0, Session::held_count);
        let place = |id, icon, tooltip, target: Placement| {
            icon_button(id, icon, tooltip)
                .selected(placement == target)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.placement = target;
                    cx.notify();
                }))
        };
        TitleBar::new()
            .bg(theme::palette(cx).card)
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .w_full()
                    .pr_2()
                    .gap_1()
                    .child(
                        icon_button(
                            "traffic-pause",
                            if state == CaptureState::Paused {
                                IconName::Play
                            } else {
                                IconName::Pause
                            },
                            if state == CaptureState::Paused {
                                "Resume recording (⌘.)"
                            } else {
                                "Pause recording (⌘.)"
                            },
                        )
                        .disabled(!running)
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_pause(cx))),
                    )
                    .child(
                        icon_button("traffic-compose", IconName::SquarePen, "Compose request")
                            .disabled(!running)
                            .on_click(cx.listener(|this, _, _, cx| this.open_editor(None, cx))),
                    )
                    .child(
                        icon_button("traffic-clear", IconName::Trash, "Clear (⌘K)")
                            .disabled(!running)
                            .on_click(cx.listener(|this, _, _, cx| this.clear(cx))),
                    )
                    .child(divider(cx))
                    .child(open_app)
                    .child(divider(cx))
                    .child(
                        icon_button("traffic-rules", IconName::ListChecks, "Rules")
                            .on_click(|_, _, cx| cx.defer(|cx| super::rules::open(None, None, cx))),
                    )
                    .child(
                        icon_button(
                            "traffic-breakpoint-switch",
                            IconName::OctagonPause,
                            if breakpoints {
                                "Breakpoints on (⌘B)"
                            } else {
                                "Breakpoints off (⌘B)"
                            },
                        )
                        .selected(breakpoints)
                        .on_click(|_, _, cx| toggle_breakpoints(cx)),
                    )
                    .child(
                        Button::new("traffic-breakpoints")
                            .ghost()
                            .small()
                            .label(if held == 0 {
                                "Breakpoints".to_owned()
                            } else {
                                format!("Breakpoints · {held}")
                            })
                            .when(held > 0, |button| {
                                button.text_color(theme::palette(cx).danger)
                            })
                            .tooltip("Held messages (⇧⌘B)")
                            .on_click(|_, _, cx| cx.defer(super::breakpoints::open)),
                    )
                    .child(div().flex_1())
                    .child(pill)
                    .child(div().flex_1())
                    .child(
                        icon_button("traffic-sidebar-toggle", IconName::PanelLeft, "Sidebar")
                            .selected(self.show_sidebar)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_sidebar = !this.show_sidebar;
                                cx.notify();
                            })),
                    )
                    .child(place(
                        "traffic-detail-below",
                        IconName::PanelBottom,
                        "Details below",
                        Placement::Below,
                    ))
                    .child(place(
                        "traffic-detail-right",
                        IconName::PanelRight,
                        "Details on the right",
                        Placement::Right,
                    ))
                    .child(place(
                        "traffic-detail-hidden",
                        IconName::PanelBottomClose,
                        "Hide details",
                        Placement::Hidden,
                    )),
            )
    }

    pub(super) fn tab_strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = self.tabs;
        let tab = |id: (&'static str, usize), name: &'static str, selected: bool| {
            Button::new(id)
                .ghost()
                .xsmall()
                .selected(selected)
                .label(name)
        };
        let scope_label = self.scope.as_ref().map(|scope| match scope {
            Scope::App(app) => app.name.clone(),
            Scope::AppDomain(app, domain) => format!("{} · {domain}", app.name),
            Scope::Domain(domain) | Scope::Host(domain) => domain.clone(),
            Scope::Rule(id) => rules(cx)
                .into_iter()
                .find(|rule| rule.id == *id)
                .map_or_else(
                    || format!("Rule {id}"),
                    |rule| format!("Rule: {}", rule.name),
                ),
        });
        h_flex()
            .id("traffic-tabs")
            .flex_shrink_0()
            .h(px(30.))
            .px_2()
            .gap_0p5()
            .overflow_x_scroll()
            .border_b_1()
            .border_color(cx.theme().border)
            .children(
                Transport::ALL
                    .into_iter()
                    .enumerate()
                    .map(|(index, transport)| {
                        tab(
                            ("traffic-transport", index),
                            transport.name(),
                            tabs.transport == transport,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.tabs.transport = transport;
                            cx.notify();
                        }))
                    }),
            )
            .child(divider(cx))
            .children(
                ContentKind::ALL
                    .into_iter()
                    .enumerate()
                    .map(|(index, kind)| {
                        tab(
                            ("traffic-content", index),
                            kind.name(),
                            tabs.content == Some(kind),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.tabs.content = (this.tabs.content != Some(kind)).then_some(kind);
                            cx.notify();
                        }))
                    }),
            )
            .child(div().flex_1().min_w(px(8.)))
            .when_some(scope_label, |row, label| {
                row.child(
                    Button::new("traffic-scope-chip")
                        .outline()
                        .xsmall()
                        .label(format!("{label} ×"))
                        .tooltip("Clear the sidebar selection")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.scope = None;
                            cx.notify();
                        })),
                )
            })
            .when_some(self.process_scope, |row, id| {
                row.child(
                    Button::new("traffic-process-scope")
                        .outline()
                        .xsmall()
                        .label(format!("PID {} ×", id.0))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.process_scope = None;
                            cx.notify();
                        })),
                )
            })
            .child(
                Button::new("traffic-reset-filters")
                    .ghost()
                    .xsmall()
                    .label("Reset filters")
                    .tooltip("Reset filters (Esc)")
                    .disabled(!self.filters_active(cx))
                    .on_click(cx.listener(|this, _, window, cx| this.reset_filters(window, cx))),
            )
    }

    pub(super) fn filter_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let status = self.status;
        let entity = cx.entity();
        let [search, process, host, method] = &self.filters;
        v_flex()
            .flex_shrink_0()
            .px_2()
            .py_1p5()
            .gap_1p5()
            .bg(theme::palette(cx).card)
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(140.))
                            .child(Input::new(search).small()),
                    )
                    .child(div().w(px(120.)).child(Input::new(process).small()))
                    .child(div().w(px(120.)).child(Input::new(host).small()))
                    .child(div().w(px(80.)).child(Input::new(method).small()))
                    .child(
                        Button::new("traffic-status-filter")
                            .outline()
                            .small()
                            .label(
                                status.map_or_else(
                                    || "Any status".into(),
                                    |class| format!("{class}xx"),
                                ),
                            )
                            .dropdown_caret(true)
                            .dropdown_menu(move |mut menu, _, _| {
                                for class in [None, Some(1), Some(2), Some(3), Some(4), Some(5)] {
                                    let entity = entity.clone();
                                    menu =
                                        menu.item(
                                            PopupMenuItem::new(class.map_or_else(
                                                || "Any status".into(),
                                                |n| format!("{n}xx"),
                                            ))
                                            .checked(status == class)
                                            .on_click(move |_, _, cx| {
                                                entity.update(cx, |this, cx| {
                                                    this.status = class;
                                                    cx.notify();
                                                })
                                            }),
                                        );
                                }
                                menu
                            }),
                    )
                    .child(
                        Checkbox::new("traffic-error-filter")
                            .small()
                            .checked(self.errors)
                            .label("Has error")
                            .on_click(cx.listener(|this, value: &bool, _, cx| {
                                this.errors = *value;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("traffic-har-filtered")
                            .outline()
                            .xsmall()
                            .label("Export filtered HAR…")
                            .disabled(self.rows.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| {
                                let ids = this
                                    .matching(cx)
                                    .iter()
                                    .map(|&index| this.rows[index].id)
                                    .collect();
                                this.export_har(ids, cx)
                            })),
                    )
                    .child(
                        Checkbox::new("traffic-har-bodies")
                            .small()
                            .checked(self.include_bodies)
                            .label("Include bodies")
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.include_bodies = *checked;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "HAR files contain captured traffic. Review them before sharing.",
                            ),
                    ),
            )
    }

    pub(super) fn bottom_bar(
        &self,
        visible: usize,
        selected_visible: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let running = self.capture.read(cx).session.is_some();
        let (up, down) = self.throughput.rates();
        let counters = self.counters;
        let muted = cx.theme().muted_foreground;
        let selected = usize::from(selected_visible);
        let warnings = [
            ("refused", counters.refused),
            ("dropped", counters.flows_dropped),
            ("bodies evicted", counters.evicted_bodies),
            ("passed the hold limit", counters.holds_skipped),
            ("timed out at breakpoints", counters.holds_timed_out),
        ]
        .into_iter()
        .filter(|(_, count)| *count > 0)
        .map(|(name, count)| format!("{count} {name}"))
        .collect::<Vec<_>>()
        .join(" · ");
        h_flex()
            .flex_shrink_0()
            .h(px(28.))
            .px_2()
            .gap_1()
            .text_xs()
            .bg(theme::palette(cx).card)
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("traffic-bottom-clear")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Trash)
                    .label("Clear")
                    .tooltip("Clear (⌘K)")
                    .disabled(!running)
                    .on_click(cx.listener(|this, _, _, cx| this.clear(cx))),
            )
            .child(
                Button::new("traffic-filter-toggle")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Funnel)
                    .label("Filter")
                    .tooltip("Filter bar (⌘F)")
                    .selected(self.show_filters)
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_filters(window, cx))),
            )
            .child(
                Button::new("traffic-auto-select")
                    .ghost()
                    .xsmall()
                    .icon(IconName::MousePointerClick)
                    .label("Auto Select")
                    .tooltip("Keep the newest flow selected")
                    .selected(self.auto_select.enabled)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.auto_select.toggle();
                        this.follow_newest(cx);
                        this.refresh_detail(cx);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("traffic-operation")
                    .flex_1()
                    .min_w_0()
                    .px_2()
                    .truncate()
                    .when_some(self.operation.as_ref(), |row, result| {
                        let (Ok(message) | Err(message)) = result;
                        // The bar truncates long messages; hover shows all of it.
                        let full = message.clone();
                        row.text_color(if result.is_ok() {
                            muted
                        } else {
                            theme::palette(cx).danger
                        })
                        .tooltip(move |window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new(full.clone())
                                .build(window, cx)
                        })
                        .child(message.clone())
                    }),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_color(muted)
                    .child(format!("{selected}/{visible} rows selected")),
            )
            .child(div().flex_1().min_w(px(8.)))
            .when(!warnings.is_empty(), |row| {
                row.child(
                    div()
                        .flex_shrink_0()
                        .text_color(theme::palette(cx).warning)
                        .child(warnings),
                )
                .child(divider(cx))
            })
            .child(div().flex_shrink_0().text_color(muted).child(format!(
                "{} of {}",
                format_bytes(counters.memory as u64),
                format_bytes(MEMORY_BUDGET)
            )))
            .child(divider(cx))
            .child(
                div()
                    .flex_shrink_0()
                    .text_color(muted)
                    .font_family(cx.theme().mono_font_family.clone())
                    .child(format!(
                        "↑ {}/s ↓ {}/s",
                        format_bytes(up),
                        format_bytes(down)
                    )),
            )
            .child(divider(cx))
            .child(
                div()
                    .flex_shrink_0()
                    .px_1p5()
                    .rounded_sm()
                    .border_1()
                    .border_color(cx.theme().border)
                    .text_color(muted)
                    .child("Manual"),
            )
    }

    pub(super) fn toggle_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_filters = !self.show_filters;
        if self.show_filters {
            self.filters[0].update(cx, |input, cx| input.focus(window, cx));
        } else {
            self.focus.focus(window, cx);
        }
        cx.notify();
    }
}
