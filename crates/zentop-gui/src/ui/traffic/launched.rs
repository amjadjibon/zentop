//! Open with Zentop in the window: the toolbar menu, launching, the Launched
//! list in Proxy setup, quit confirmations, and the watcher that notices
//! launched apps exiting and deletes their temporary profiles.
use super::launcher::{self, AppKind, InstalledApp};
use super::*;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::WindowExt;
use gpui_kit::FontWeight;
use std::path::PathBuf;

/// How long Zentop waits for an already running app to quit when asked.
const QUIT_WAIT: Duration = Duration::from_secs(10);
/// An app that exits this soon after opening likely refused its arguments.
const EARLY_EXIT: Duration = Duration::from_secs(3);

/// An app Zentop launched, kept until it exits.
pub(super) struct Launched {
    pub app: InstalledApp,
    pub pid: u32,
    pub unique_id: u64,
    /// The family registered with the session it was launched into.
    pub family: Option<u64>,
    pub generation: u64,
    /// The temporary browser profile, deleted once the app exits.
    pub profile: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LaunchState {
    Capturing,
    NotCapturing,
}

impl Capture {
    pub(super) fn launch_state(&self, launched: &Launched) -> LaunchState {
        let registered = self
            .session
            .as_ref()
            .filter(|_| self.generation == launched.generation)
            .zip(launched.family)
            .is_some_and(|(session, id)| {
                session
                    .launch_access()
                    .launched()
                    .iter()
                    .any(|app| app.id == id)
            });
        if registered {
            LaunchState::Capturing
        } else {
            LaunchState::NotCapturing
        }
    }

    /// Launched apps still running that can't reach the network any more.
    pub(super) fn stranded(&self) -> Vec<(String, u32)> {
        self.launched
            .iter()
            .filter(|launched| self.launch_state(launched) == LaunchState::NotCapturing)
            .map(|launched| (launched.app.name.clone(), launched.pid))
            .collect()
    }
}

/// The icon of an app opened with Zentop, by the name its rows carry.
pub(super) fn launched_icon(
    name: Option<&str>,
    traffic: &Traffic,
    cx: &App,
) -> Option<crate::native_icons::Icon> {
    traffic.capture.read(cx).launched_icons.get(name?).cloned()
}

/// What to say when an app quits right after Zentop opened it.
pub(super) fn early_exit_message(name: &str, spki: bool) -> String {
    if spki {
        format!(
            "{name} quit right after opening. Some apps refuse Chromium's certificate switch; \
             turn off \"Accept Zentop's certificate in launched apps\" and open it again."
        )
    } else {
        format!("{name} quit right after opening. It may not accept Chromium's proxy switch.")
    }
}

/// Finds launchable apps, with icons, off the UI thread.
pub(super) fn discover(capture: &Entity<Capture>, cx: &mut App) {
    if capture.read(cx).discovering {
        return;
    }
    capture.update(cx, |capture, _| capture.discovering = true);
    let state = monitor::global(cx).read(cx).state();
    cx.spawn({
        let capture = capture.clone();
        async move |cx| {
            let apps = cx
                .background_executor()
                .spawn(async move {
                    let mut apps = launcher::discover(&launcher::search_roots());
                    if let Ok(mut icons) = state.icons.lock() {
                        for app in &mut apps {
                            app.icon = icons.resolve(&app.path);
                        }
                    }
                    apps
                })
                .await;
            capture.update(cx, |capture, cx| {
                capture.apps = apps;
                capture.discovering = false;
                cx.notify();
            });
        }
    })
    .detach();
}

/// Deletes temporary profiles left by a Zentop that is gone, once their
/// browsers are gone too.
pub(super) fn clean_stale_profiles(cx: &mut App) {
    cx.background_executor()
        .spawn(async {
            let root = launcher::profiles_root();
            for profile in launcher::stale_profiles(&root, launcher::pid_alive) {
                launcher::remove_profile(&root, &profile);
            }
        })
        .detach();
}

/// Polls launched apps once a second while any are listed. An app that
/// exited is dropped and its profile deleted.
fn watch(capture: &Entity<Capture>, cx: &mut App) {
    if capture.read(cx).watcher.is_some() {
        return;
    }
    let task = cx.spawn({
        let capture = capture.downgrade();
        async move |cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let Ok(done) = capture.update(cx, |capture, cx| {
                let before = capture.launched.len();
                let mut profiles = Vec::new();
                capture.launched.retain_mut(|launched| {
                    let alive =
                        zentop_proxy::process_unique_id(launched.pid) == Some(launched.unique_id);
                    if !alive {
                        profiles.extend(launched.profile.take());
                    }
                    alive
                });
                if !profiles.is_empty() {
                    cx.background_executor()
                        .spawn(async move {
                            let root = launcher::profiles_root();
                            for profile in profiles {
                                launcher::remove_profile(&root, &profile);
                            }
                        })
                        .detach();
                }
                if capture.launched.len() != before {
                    cx.notify();
                }
                let done = capture.launched.is_empty();
                if done {
                    capture.watcher = None;
                }
                done
            }) else {
                break;
            };
            if done {
                break;
            }
        }
    });
    capture.update(cx, |capture, _| capture.watcher = Some(task));
}

/// Asks once, then quits each app gracefully. Never sends a signal.
fn confirm_quit(apps: Vec<(String, u32)>, window: &mut Window, cx: &mut App) {
    if apps.is_empty() {
        return;
    }
    let title = match apps.as_slice() {
        [(name, _)] => format!("Quit {name}?"),
        _ => format!("Quit {} launched apps?", apps.len()),
    };
    let list = apps
        .iter()
        .map(|(name, pid)| format!("{name} · PID {pid}"))
        .collect::<Vec<_>>()
        .join("\n");
    window.open_alert_dialog(cx, move |dialog, _, _| {
        let apps = apps.clone();
        dialog
            .title(title.clone())
            .description(
                "Zentop asks each app to quit, as its Quit command would. Unsaved work may \
                 be lost, and an app may ask you to save first.",
            )
            .child(div().p_3().text_xs().child(list.clone()))
            .ok_text("Quit")
            .ok_variant(ButtonVariant::Danger)
            .confirm()
            .on_ok(move |_, window, cx| {
                let failed: Vec<_> = apps
                    .iter()
                    .filter(|(_, pid)| !launcher::quit(*pid))
                    .map(|(name, _)| name.clone())
                    .collect();
                if !failed.is_empty() {
                    window.push_notification(
                        gpui_kit::component::notification::Notification::error(format!(
                            "Could not ask {} to quit. It may have exited already.",
                            failed.join(", ")
                        )),
                        cx,
                    );
                }
                true
            })
    });
}

impl Traffic {
    pub(super) fn open_app_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let capture = self.capture.read(cx);
        let apps = capture.apps.clone();
        let discovering = capture.discovering;
        let spki = capture.launch_spki;
        let spki_available = capture.certificates.usable();
        let entity = cx.entity();
        Button::new("traffic-open-app")
            .ghost()
            .small()
            .icon(IconName::AppWindowMac)
            .label("Open App")
            .tooltip("Open an app with Zentop")
            .dropdown_menu(move |mut menu, _, _| {
                menu = menu.max_h(px(520.)).scrollable(true);
                if apps.is_empty() {
                    menu = menu.label(if discovering {
                        "Finding apps…"
                    } else {
                        "No Chromium browsers or Electron apps found"
                    });
                }
                let mut kind = None;
                for app in &apps {
                    if kind != Some(app.kind) {
                        kind = Some(app.kind);
                        menu = menu.label(match app.kind {
                            AppKind::Browser => "Browsers · temporary profile",
                            AppKind::Electron => "Electron apps",
                            AppKind::Other => "Apps",
                        });
                    }
                    let shown = app.clone();
                    let launched = app.clone();
                    let entity = entity.clone();
                    menu = menu.item(
                        PopupMenuItem::element(move |_, cx| app_label(&shown, cx)).on_click(
                            move |_, window, cx| {
                                entity.update(cx, |this, cx| {
                                    this.open_app(launched.clone(), window, cx)
                                })
                            },
                        ),
                    );
                }
                let choose = entity.clone();
                let toggle = entity.clone();
                menu.separator()
                    .item(
                        PopupMenuItem::new("Choose App…").on_click(move |_, window, cx| {
                            choose.update(cx, |this, cx| this.choose_app(window, cx))
                        }),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new(if spki_available {
                            "Accept Zentop's certificate in launched apps"
                        } else {
                            "Accept Zentop's certificate (create it in Proxy setup first)"
                        })
                        .checked(spki && spki_available)
                        .disabled(!spki_available)
                        .on_click(move |_, _, cx| {
                            toggle.update(cx, |this, cx| {
                                this.capture.update(cx, |capture, cx| {
                                    capture.launch_spki = !capture.launch_spki;
                                    cx.notify();
                                });
                                // The menu is built from what the window last rendered.
                                cx.notify();
                            })
                        }),
                    )
            })
    }

    fn choose_app(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open with Zentop".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| match launcher::inspect(&path) {
                Some(app) => this.open_app(app, window, cx),
                None => {
                    this.operation = Some(Err(format!("{} is not an app", path.display())));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Warns about apps that may ignore the switch, asks before quitting a
    /// running single-instance app, then launches.
    pub(super) fn open_app(
        &mut self,
        app: InstalledApp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if app.kind == AppKind::Other {
            let entity = cx.entity();
            let name = app.name.clone();
            window.open_alert_dialog(cx, move |dialog, _, _| {
                let entity = entity.clone();
                let app = app.clone();
                dialog
                    .title(format!("Open {name} with Zentop?"))
                    .description(
                        "This app is neither a Chromium browser nor an Electron app. It may \
                         ignore the --proxy-server switch, and then nothing is captured.",
                    )
                    .ok_text("Open Anyway")
                    .confirm()
                    .on_ok(move |_, window, cx| {
                        let app = app.clone();
                        entity.update(cx, |this, cx| this.confirm_running(app, window, cx));
                        true
                    })
            });
            return;
        }
        self.confirm_running(app, window, cx);
    }

    fn confirm_running(&mut self, app: InstalledApp, window: &mut Window, cx: &mut Context<Self>) {
        // Browsers run as a separate instance with their own profile.
        let running = if app.kind == AppKind::Browser {
            Vec::new()
        } else {
            launcher::running(&app.bundle_id)
        };
        if running.is_empty() {
            self.begin_launch(app, Vec::new(), cx);
            return;
        }
        let entity = cx.entity();
        let name = app.name.clone();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let entity = entity.clone();
            let app = app.clone();
            let running = running.clone();
            dialog
                .title(format!("{name} is already running"))
                .description(format!(
                    "{name} runs one instance at a time, so it must quit and reopen to use \
                     Zentop's proxy. Zentop asks it to quit, as its Quit command would; \
                     unsaved work may be lost."
                ))
                .ok_text(format!("Quit and Reopen {name}"))
                .ok_variant(ButtonVariant::Danger)
                .confirm()
                .on_ok(move |_, _, cx| {
                    let app = app.clone();
                    let running = running.clone();
                    entity.update(cx, |this, cx| this.begin_launch(app, running, cx));
                    true
                })
        });
    }

    fn begin_launch(&mut self, app: InstalledApp, quit: Vec<u32>, cx: &mut Context<Self>) {
        if self.capture.read(cx).session.is_none() {
            self.start_or_stop(cx);
        }
        let capture = self.capture.read(cx);
        let Some(session) = capture.session.as_ref() else {
            self.operation = Some(Err(self
                .error
                .clone()
                .unwrap_or_else(|| "Start capture first".into())));
            cx.notify();
            return;
        };
        let spki = if capture.launch_spki && capture.certificates.usable() {
            match capture
                .decryption
                .as_ref()
                .map(|tls| tls.session_key_spki())
            {
                Some(Ok(spki)) => Some(spki),
                Some(Err(error)) => {
                    self.operation = Some(Err(format!("Could not create a session key: {error}")));
                    cx.notify();
                    return;
                }
                None => {
                    self.operation = Some(Err(
                        "Decryption is unavailable for this capture. Restart capture to accept \
                         Zentop's certificate in launched apps."
                            .into(),
                    ));
                    cx.notify();
                    return;
                }
            }
        } else {
            None
        };
        let address = session.address();
        let access = session.launch_access();
        let generation = capture.generation;
        let with_spki = spki.is_some();
        self.operation = Some(Ok(format!("Opening {}…", app.name)));
        cx.notify();
        let capture = self.capture.clone();
        cx.spawn(async move |this, cx| {
            let launching = app.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let app = launching;
                    if !quit.is_empty() {
                        for pid in &quit {
                            launcher::quit(*pid);
                        }
                        let deadline = std::time::Instant::now() + QUIT_WAIT;
                        while quit.iter().any(|pid| launcher::pid_alive(*pid)) {
                            if std::time::Instant::now() >= deadline {
                                return Err(format!(
                                    "{} didn't quit. Quit it yourself, then try again.",
                                    app.name
                                ));
                            }
                            std::thread::sleep(Duration::from_millis(100));
                        }
                    }
                    // From here until the family is registered, the app's
                    // first connections wait for it instead of getting 407.
                    let _guard = access.expect_launch();
                    let root = launcher::profiles_root();
                    let profile = (app.kind == AppKind::Browser)
                        .then(|| launcher::new_profile(&root))
                        .transpose()
                        .map_err(|error| {
                            format!("Could not create a temporary profile: {error}")
                        })?;
                    let arguments = launcher::launch_arguments(
                        app.kind,
                        address,
                        profile.as_deref(),
                        spki.as_deref(),
                    );
                    let pid = match launcher::launch(&app.path, &arguments) {
                        Ok(pid) => pid,
                        Err(error) => {
                            if let Some(profile) = &profile {
                                launcher::remove_profile(&root, profile);
                            }
                            return Err(format!("Could not open {}: {error}", app.name));
                        }
                    };
                    let unique_id = zentop_proxy::process_unique_id(pid);
                    let registered = access.register(pid, &app.name, &app.path);
                    Ok((pid, unique_id, profile, registered))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.operation = Some(match result {
                    Ok((pid, Some(unique_id), profile, registered)) => {
                        let message = match &registered {
                            Ok(_) => Ok(format!("Opened {} with Zentop", app.name)),
                            Err(error) => Err(format!("{error}. Its traffic needs the token.")),
                        };
                        capture.update(cx, |capture, cx| {
                            if let Some(icon) = &app.icon {
                                capture
                                    .launched_icons
                                    .insert(app.name.clone(), icon.clone());
                            }
                            capture.launched.push(Launched {
                                app: app.clone(),
                                pid,
                                unique_id,
                                family: registered.ok().map(|launched| launched.id),
                                generation,
                                profile,
                            });
                            cx.notify();
                        });
                        watch(&capture, cx);
                        this.check_early_exit(app.name.clone(), pid, unique_id, with_spki, cx);
                        message
                    }
                    Ok((_, None, profile, _)) => {
                        if let Some(profile) = profile {
                            launcher::remove_profile(&launcher::profiles_root(), &profile);
                        }
                        Err(format!("{} exited right after opening", app.name))
                    }
                    Err(error) => Err(error),
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// Says so when a launched app quits within a few seconds, instead of
    /// letting it silently leave the Launched list.
    fn check_early_exit(
        &mut self,
        name: String,
        pid: u32,
        unique_id: u64,
        spki: bool,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(EARLY_EXIT).await;
            if zentop_proxy::process_unique_id(pid) == Some(unique_id) {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                this.operation = Some(Err(early_exit_message(&name, spki)));
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn forget_launched(&mut self, pid: u32, cx: &mut Context<Self>) {
        let capture = self.capture.read(cx);
        if let (Some(session), Some(launched)) = (
            capture.session.as_ref(),
            capture.launched.iter().find(|launched| launched.pid == pid),
        ) {
            if let Some(id) = launched.family {
                session.launch_access().forget(id);
            }
        }
        cx.notify();
    }

    /// The Launched list in Proxy setup.
    pub(super) fn launched_panel(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let capture = self.capture.read(cx);
        if capture.launched.is_empty() {
            return None;
        }
        let rows: Vec<_> = capture
            .launched
            .iter()
            .map(|launched| {
                (
                    launched.app.clone(),
                    launched.pid,
                    capture.launch_state(launched),
                )
            })
            .collect();
        let muted = cx.theme().muted_foreground;
        let running = theme::palette(cx).running;
        Some(
            v_flex()
                .gap_1()
                .children(rows.into_iter().map(|(app, pid, state)| {
                    let name = app.name.clone();
                    h_flex()
                        .gap_2()
                        .text_xs()
                        .child(app_icon(&app, px(16.)))
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .min_w_0()
                                .truncate()
                                .child(app.name.clone()),
                        )
                        .child(div().text_color(muted).child(format!("PID {pid}")))
                        .child(
                            div()
                                .text_color(if state == LaunchState::Capturing {
                                    running
                                } else {
                                    muted
                                })
                                .child(match state {
                                    LaunchState::Capturing => "Capturing",
                                    LaunchState::NotCapturing => "Not capturing",
                                }),
                        )
                        .child(div().flex_1())
                        .when(state == LaunchState::Capturing, |row| {
                            row.child(
                                Button::new(("traffic-launched-forget", pid as usize))
                                    .ghost()
                                    .xsmall()
                                    .label("Stop Capturing")
                                    .tooltip("New connections from this app get 407")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.forget_launched(pid, cx)
                                    })),
                            )
                        })
                        .child(
                            Button::new(("traffic-launched-quit", pid as usize))
                                .outline()
                                .xsmall()
                                .label("Quit…")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    // A dialog opens under the popover, so close it first.
                                    this.setup_open = false;
                                    cx.notify();
                                    confirm_quit(vec![(name.clone(), pid)], window, cx)
                                })),
                        )
                })),
        )
    }

    /// Shown when capture stopped while launched apps still run.
    pub(super) fn launched_notice(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let capture = self.capture.read(cx);
        if capture.session.is_some() {
            return None;
        }
        let stranded = capture.stranded();
        if stranded.is_empty() {
            return None;
        }
        let text = match stranded.as_slice() {
            [(name, _)] => format!(
                "{name} was opened with Zentop and can't reach the network until you quit it \
                 and open it normally."
            ),
            _ => format!(
                "{} apps were opened with Zentop and can't reach the network until you quit \
                 them and open them normally.",
                stranded.len()
            ),
        };
        Some(
            h_flex()
                .flex_shrink_0()
                .px_3()
                .py_1p5()
                .gap_2()
                .text_xs()
                .bg(theme::palette(cx).warning.opacity(0.12))
                .border_b_1()
                .border_color(cx.theme().border)
                .child(Icon::new(IconName::TriangleAlert).size_3p5())
                .child(div().flex_1().child(text))
                .child(
                    Button::new("traffic-quit-launched")
                        .outline()
                        .xsmall()
                        .label("Quit Launched Apps…")
                        .on_click(move |_, window, cx| confirm_quit(stranded.clone(), window, cx)),
                ),
        )
    }
}

fn app_icon(app: &InstalledApp, size: gpui_kit::Pixels) -> AnyElement {
    match &app.icon {
        Some(image) => gpui_kit::img(image.clone()).size(size).into_any_element(),
        None => Icon::new(IconName::AppWindowMac)
            .size(size)
            .into_any_element(),
    }
}

fn app_label(app: &InstalledApp, cx: &App) -> impl IntoElement {
    h_flex()
        .gap_2()
        .min_w(px(280.))
        .child(app_icon(app, px(18.)))
        .child(div().flex_1().child(app.name.clone()))
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(app.kind.label()),
        )
}

#[cfg(test)]
mod tests {
    use super::early_exit_message;

    #[test]
    fn early_exit_points_at_the_certificate_switch_only_when_it_was_used() {
        let spki = early_exit_message("Claude", true);
        assert!(spki.starts_with("Claude quit right after opening."));
        assert!(spki.contains("Accept Zentop's certificate in launched apps"));
        let plain = early_exit_message("Claude", false);
        assert!(!plain.contains("certificate"));
        assert!(plain.contains("proxy switch"));
    }
}
