use super::*;

use std::{path::PathBuf, sync::Mutex};
use zentop_proxy::{
    decrypt_host,
    keychain::{CertificateAction, CertificateState, CertificateStatus, Certificates},
    CertificateAuthority, Decryption,
};

#[derive(Default)]
pub(super) struct CertificateSetup {
    manager: Arc<Mutex<Option<Certificates>>>,
    status: Option<CertificateStatus>,
    authority: Option<CertificateAuthority>,
    busy: bool,
    error: Option<String>,
}

impl CertificateSetup {
    /// Whether a root is available to issue leaves this session.
    pub(super) fn usable(&self) -> bool {
        self.authority.is_some()
    }

    pub(super) fn bind(&self, hosts: Vec<String>) -> std::io::Result<Arc<Decryption>> {
        let decryption = Arc::new(Decryption::platform()?);
        decryption.set_hosts(hosts);
        decryption.set_authority(self.authority.clone());
        if let Some(reason) = unavailable_reason(self.status.as_ref()) {
            decryption.disable_for_session(reason);
        }
        Ok(decryption)
    }
}

fn unavailable_reason(status: Option<&CertificateStatus>) -> Option<String> {
    let status = status.filter(|status| status.state == CertificateState::KeyUnavailable)?;
    Some(status.explanation.first().map_or_else(
        || "Certificate key unavailable".into(),
        |reason| format!("Certificate key unavailable: {reason}"),
    ))
}

fn run_action(capture: &Entity<Capture>, action: CertificateAction, cx: &mut App) {
    capture.update(cx, |this, cx| {
        if this.certificates.busy {
            return;
        }
        this.certificates.busy = true;
        this.certificates.error = None;
        let manager = this.certificates.manager.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let mut manager = manager.lock().unwrap();
                    if manager.is_none() {
                        let path = zentop_core::monitoring_preferences::config_directory()
                            .ok_or("Configuration directory unavailable".to_owned())?;
                        *manager =
                            Some(Certificates::open(&path).map_err(|error| error.to_string())?);
                    }
                    let Some(manager) = manager.as_mut() else {
                        return Err("Could not open certificate storage".into());
                    };
                    let error = manager.perform(action).err().map(|error| error.to_string());
                    let (status, authority) = manager.inspect();
                    Ok((status, authority, error))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.certificates.busy = false;
                match result {
                    Ok((status, authority, error)) => {
                        if let Some(decryption) = &this.decryption {
                            decryption.set_authority(authority.clone());
                            if let Some(reason) = unavailable_reason(Some(&status)) {
                                decryption.disable_for_session(reason);
                            }
                        }
                        this.certificates.status = Some(status);
                        this.certificates.authority = authority;
                        this.certificates.error = error;
                    }
                    Err(error) => this.certificates.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    });
}

pub(super) fn load(capture: &Entity<Capture>, cx: &mut App) {
    if capture.read(cx).certificates.status.is_none() {
        run_action(capture, CertificateAction::Refresh, cx);
    }
}

pub(super) fn hosts(cx: &App) -> Vec<String> {
    monitor::global(cx)
        .read(cx)
        .prefs()
        .behavior("trafficDecryptHosts")
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|host| host.as_str().map(str::to_owned))
        .collect()
}

impl Traffic {
    pub(super) fn edit_host(&mut self, raw: &str, add: bool, cx: &mut Context<Self>) {
        let host = match decrypt_host(raw) {
            Ok(host) => host,
            Err(error) => {
                self.error = Some(error.into());
                cx.notify();
                return;
            }
        };
        let mut hosts = hosts(cx);
        if add {
            if !hosts.contains(&host) {
                hosts.push(host);
                hosts.sort_unstable();
            }
        } else {
            hosts.retain(|entry| *entry != host);
        }
        let monitor = monitor::global(cx);
        monitor.update(cx, |monitor, cx| {
            monitor.save(
                serde_json::json!({"behavior":{"trafficDecryptHosts":hosts}}),
                cx,
            );
        });
        self.error = monitor.read(cx).prefs_error.clone();
        if self.error.is_none() {
            if let Some(decryption) = &self.capture.read(cx).decryption {
                decryption.set_hosts(hosts);
            }
        }
        cx.notify();
    }

    fn export_certificate(&mut self, pem: String, cx: &mut Context<Self>) {
        let directory = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
        let selected = cx.prompt_for_new_path(&directory, Some("zentop-ca.pem"));
        cx.spawn(async move |this, cx| {
            let result = match selected.await {
                Ok(Ok(Some(path))) => {
                    cx.background_executor()
                        .spawn(async move {
                            std::fs::write(&path, pem)
                                .map(|()| path)
                                .map_err(|error| error.to_string())
                        })
                        .await
                }
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(error)) => Err(error.to_string()),
            };
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(path) => {
                        this.exported_pem = Some(path);
                        this.error = None;
                    }
                    Err(error) => {
                        this.error = Some(format!("Could not export certificate: {error}"))
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn certificate_panel(&self, cx: &mut Context<Self>) -> Div {
        let capture = self.capture.read(cx);
        let setup = &capture.certificates;
        let status = setup.status.as_ref();
        let session_error = capture
            .decryption
            .as_ref()
            .and_then(|decryption| decryption.error());
        let label = match status.map(|status| status.state) {
            None if setup.busy => "Checking Keychain…".into(),
            None => "Certificate status unavailable".into(),
            Some(CertificateState::None) => "No certificate".into(),
            Some(CertificateState::Created) => "Created · not trusted".into(),
            Some(CertificateState::Trusted) => format!(
                "Trusted · expires {}",
                status
                    .and_then(|s| s.expires.as_deref())
                    .unwrap_or("unknown")
            ),
            Some(CertificateState::KeyUnavailable) => "Certificate key unavailable".into(),
            Some(CertificateState::Expiring) => format!(
                "Expires {}",
                status
                    .and_then(|s| s.expires.as_deref())
                    .unwrap_or("unknown")
            ),
            Some(CertificateState::PartlyRemoved) => "Partly removed".into(),
        };
        let hosts = hosts(cx);
        let has_error = setup.error.is_some() || session_error.is_some();
        let controls = h_flex()
            .gap_2()
            .child(
                Button::new("traffic-certificate-toggle")
                    .ghost()
                    .small()
                    // The popover's section heading already names this panel.
                    .label(if self.show_certificate {
                        "Hide details"
                    } else {
                        "Show details"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_certificate = !this.show_certificate;
                        cx.notify();
                    })),
            )
            .child(div().text_xs().child(label))
            .when(!hosts.is_empty(), |row| {
                row.child(div().text_xs().child(match hosts.len() {
                    1 => "1 decrypt host".to_string(),
                    count => format!("{count} decrypt hosts"),
                }))
            })
            .when(has_error, |row| {
                row.child(
                    div()
                        .text_xs()
                        .text_color(theme::palette(cx).warning)
                        .child(if session_error.is_some() {
                            "Decryption off for this session"
                        } else {
                            "Certificate action needs attention"
                        }),
                )
            });
        let section = v_flex()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .child(controls)
            .when_some(session_error, |panel, error| {
                panel.child(
                    div()
                        .text_xs()
                        .text_color(theme::palette(cx).warning)
                        .child(format!(
                            "{error}. Restart capture after restoring Keychain access."
                        )),
                )
            });
        if !self.show_certificate {
            return section;
        }
        let state = status.map(|s| s.state);
        let trust_available = status.is_some_and(|s| s.trust_available);
        let busy = setup.busy;
        let error = setup.error.clone();
        let pem = status.and_then(|s| s.pem.clone());
        let fingerprint = status.and_then(|s| s.fingerprint.clone());
        let explanations = status.map_or_else(Vec::new, |s| s.explanation.clone());
        let action = |button: &'static str, title: &'static str, action| {
            let capture = self.capture.clone();
            Button::new(button)
                .outline()
                .small()
                .label(title)
                .disabled(busy)
                .on_click(move |_, _, cx| run_action(&capture, action, cx))
        };
        let actions = h_flex()
            .gap_2()
            .when(state.is_none(), |row| {
                row.child(action("ca-refresh", "Retry", CertificateAction::Refresh))
            })
            .when(state == Some(CertificateState::None), |row| {
                row.child(action("ca-create", "Create", CertificateAction::Create))
            })
            .when(
                state == Some(CertificateState::Created) && trust_available,
                |row| row.child(action("ca-trust", "Trust", CertificateAction::Trust)),
            )
            .when(state == Some(CertificateState::KeyUnavailable), |row| {
                row.child(action(
                    "ca-retry",
                    "Unlock and retry",
                    CertificateAction::RetryKey,
                ))
            })
            .when(
                matches!(
                    state,
                    Some(CertificateState::KeyUnavailable | CertificateState::Expiring)
                ) && trust_available,
                |row| row.child(action("ca-replace", "Replace", CertificateAction::Replace)),
            )
            .when(
                state.is_some_and(|state| state != CertificateState::None),
                |row| {
                    row.child(action(
                        "ca-remove",
                        if state == Some(CertificateState::PartlyRemoved) {
                            "Retry removal"
                        } else {
                            "Remove"
                        },
                        CertificateAction::Remove,
                    ))
                },
            )
            .when_some(pem, |row, pem| {
                row.child(
                    Button::new("ca-export")
                        .outline()
                        .small()
                        .label("Export certificate")
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.export_certificate(pem.clone(), cx)
                        })),
                )
            });
        let note_color = cx.theme().muted_foreground;
        section.child(v_flex().id("traffic-certificate-scroll").max_h(px(225.)).overflow_y_scroll()
            .gap_2().py_2().text_xs()
            .child(actions)
            .when(!trust_available && state.is_some(), |panel| panel.child(div().text_color(note_color)
                .child("Isolated Keychain. Export the certificate to use it with individual tools.")))
            .when_some(error, |panel, error| panel.child(div().text_color(theme::palette(cx).danger).child(error)))
            .when_some(fingerprint, |panel, hash| panel.child(copy_line("ca-fingerprint", "SHA-256", hash, cx)))
            .children(explanations.into_iter().map(|line| div().child(line)))
            .when_some(self.exported_pem.as_ref(), |panel, path| {
                let path = format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
                panel.child(copy_line("ca-curl", "curl", format!("curl -x \"$http_proxy\" --cacert {path} https://example.com"), cx))
                    .child(copy_line("ca-node", "Node.js", format!("export NODE_EXTRA_CA_CERTS={path}"), cx))
                    .child(copy_line("ca-ssl", "OpenSSL", format!("export SSL_CERT_FILE={path}"), cx))
                    .child(copy_line("ca-requests", "Requests", format!("export REQUESTS_CA_BUNDLE={path}"), cx))
            })
            .child(h_flex().gap_2()
                .child(div().w(px(240.)).child(Input::new(&self.decrypt_input).small()))
                .child(Button::new("decrypt-add").outline().small().label("Add host").on_click(cx.listener(|this, _, window, cx| {
                    let raw = this.decrypt_input.read(cx).value().to_string();
                    this.edit_host(&raw, true, cx);
                    if this.error.is_none() {
                        this.decrypt_input.update(cx, |input, cx| input.set_value("", window, cx));
                    }
                }))))
            .child(div().text_color(note_color).child("Only exact hosts are decrypted. The built-in skip list for Apple and some pinned services is best-effort."))
            .children(hosts.into_iter().enumerate().map(|(index, host)| {
                let remove = host.clone();
                h_flex().gap_2().child(div().min_w_0().truncate().child(host))
                    .child(Button::new(("decrypt-remove", index)).ghost().small().label("Remove host")
                        .on_click(cx.listener(move |this, _, _, cx| this.edit_host(&remove, false, cx))))
            })))
    }
}
