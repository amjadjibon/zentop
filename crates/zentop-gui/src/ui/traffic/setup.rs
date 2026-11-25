//! The status pill and its Proxy setup popover: Start/Stop, the address,
//! token and routing lines, fixed port, local destinations, and the
//! certificate and decrypt-list section.
use super::*;
use gpui_kit::component::popover::Popover;
use gpui_kit::{Anchor, FontWeight};
use zentop_proxy::Options;

/// How the window labels capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CaptureState {
    Capturing,
    Paused,
    Stopped,
}

impl Traffic {
    pub(super) fn capture_state(&self, cx: &App) -> CaptureState {
        match &self.capture.read(cx).session {
            None => CaptureState::Stopped,
            Some(_) if self.counters.paused => CaptureState::Paused,
            Some(_) => CaptureState::Capturing,
        }
    }

    pub(super) fn status_pill(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.capture_state(cx);
        let address = self
            .capture
            .read(cx)
            .session
            .as_ref()
            .map(|session| session.address().to_string());
        let (color, label) = match (state, address) {
            (CaptureState::Capturing, Some(address)) => {
                (theme::palette(cx).running, format!("Capturing · {address}"))
            }
            (CaptureState::Paused, Some(address)) => {
                (theme::palette(cx).warning, format!("Paused · {address}"))
            }
            _ => (cx.theme().muted_foreground, "Stopped".to_owned()),
        };
        let entity = cx.entity();
        let toggle = entity.clone();
        Popover::new("traffic-setup")
            .anchor(Anchor::TopCenter)
            .open(self.setup_open)
            .on_open_change(move |open, _, cx| {
                toggle.update(cx, |this, cx| {
                    this.setup_open = *open;
                    cx.notify();
                })
            })
            .trigger(
                Button::new("traffic-status-pill")
                    .outline()
                    .small()
                    .rounded(gpui_kit::component::button::ButtonRounded::Large)
                    .tooltip("Proxy setup")
                    .child(
                        h_flex()
                            .gap_1p5()
                            .child(div().size(px(8.)).rounded_full().bg(color))
                            .child(
                                div()
                                    .text_xs()
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .child(label),
                            )
                            .child(
                                Icon::new(IconName::ChevronDown)
                                    .size_3()
                                    .text_color(cx.theme().muted_foreground),
                            ),
                    ),
            )
            .content(move |_, _, cx| {
                entity.update(cx, |this, cx| this.setup_panel(cx).into_any_element())
            })
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
                    session.set_rules(capture.rule_set());
                    capture.session = Some(session);
                    capture.decryption = decryption;
                    cx.notify();
                });
            }
            Err(error) => self.error = Some(error.to_string()),
        }
        cx.notify();
    }

    pub(super) fn start_or_stop(&mut self, cx: &mut Context<Self>) {
        if self.capture.read(cx).session.is_some() {
            stop(cx);
            return;
        }
        match self.configured_port(cx) {
            Ok(port) => self.start(port, cx),
            Err(error) => {
                self.error = Some(error.into());
                self.setup_open = true;
                cx.notify();
            }
        }
    }

    pub(super) fn command(
        &mut self,
        command: impl FnOnce(&Session) -> bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let sent = self.capture.read(cx).session.as_ref().is_some_and(command);
        if !sent {
            self.operation = Some(Err("Capture is busy. Try again.".into()));
        }
        cx.notify();
        sent
    }

    pub(super) fn toggle_pause(&mut self, cx: &mut Context<Self>) {
        let paused = self.counters.paused;
        self.command(|session| session.pause(!paused), cx);
    }

    pub(super) fn clear(&mut self, cx: &mut Context<Self>) {
        if self.command(Session::clear, cx) {
            self.select(None);
        }
    }

    fn setup_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
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
        let counters = self.counters;
        let section = |title: &'static str| {
            div()
                .pt_1()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().muted_foreground)
                .child(title)
        };
        v_flex()
            .id("traffic-setup-panel")
            .w(px(580.))
            .max_h(px(600.))
            .overflow_y_scroll()
            .gap_2()
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
                            .on_click(cx.listener(|this, _, _, cx| this.start_or_stop(cx))),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Proxy setup"),
                    )
                    .child(div().flex_1())
                    .child(
                        Checkbox::new("traffic-allow-local")
                            .small()
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
            )
            .when_some(address, |panel, (address, token, proxy)| {
                panel
                    .child(copy_line("traffic-address", "Address", address, cx))
                    .child(copy_line("traffic-token", "Token", token, cx))
                    .child(section("Route a program"))
                    .child(
                        v_flex()
                            .gap_0p5()
                            .p_2()
                            .rounded_md()
                            .bg(cx.theme().muted)
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
            .when(!running, |panel| {
                panel.child(
                    h_flex()
                        .gap_2()
                        .child(div().text_xs().child("Fixed port"))
                        .child(div().w(px(120.)).child(Input::new(&self.port).small()))
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
            .when_some(self.error.clone(), |panel, error| {
                panel.child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
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
            .when(running, |panel| {
                panel.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                        "Refused {} · Flows dropped {} · Unattributed flows {} · Bodies evicted {}",
                        counters.refused,
                        counters.flows_dropped,
                        counters.unattributed,
                        counters.evicted_bodies,
                    )),
                )
            })
            .child(section("Certificate and hosts"))
            .child(self.certificate_panel(cx))
    }
}
