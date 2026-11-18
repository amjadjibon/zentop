//! Read-only fleet overview and recent process samples.
use super::{monitor, theme};
use crate::ssh::{Action, Connection, Phase, Reading, Readings};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    assets::IconName,
    component::{
        button::{Button, ButtonVariants},
        h_flex,
        input::{Input, InputEvent, InputState},
        v_flex, ActiveTheme, Icon, Sizable, TitleBar,
    },
    div, px, relative, App, AppContext, Context, Entity, FontWeight, InteractiveElement,
    IntoElement, ParentElement, Render, StatefulInteractiveElement, Styled, Subscription, Task,
    Window,
};
use std::time::{Duration, Instant};
use zentop_core::{
    process_view::{format_bytes, format_uptime},
    sampling,
    ssh_hosts::{Host, Store},
    ssh_stream::{ContainerHealth, ContainerState, Docker, Metrics, MAX_PROCESSES},
};

const WIDTHS: [f32; 5] = [80., 128., 128., 100., 156.];

struct Remote {
    host: Host,
    connection: Connection,
}

pub struct SshHosts {
    monitor: Entity<monitor::Monitor>,
    store: Store,
    remotes: Vec<Remote>,
    selected: Option<String>,
    selected_reading: Option<Instant>,
    show_form: bool,
    name: Entity<InputState>,
    destination: Entity<InputState>,
    error: Option<String>,
    _subscriptions: [Subscription; 3],
    _tick: Task<()>,
}

impl SshHosts {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let monitor = monitor::global(cx);
        let (store, error) = Store::open();
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Home server"));
        let destination =
            cx.new(|cx| InputState::new(window, cx).placeholder("home-server or user@host"));
        let subscriptions = [
            cx.observe(&monitor, |_, _, cx| cx.notify()),
            cx.subscribe_in(
                &destination,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) && this.show_form {
                        this.add_host(window, cx);
                    }
                },
            ),
            cx.on_app_quit(|this, _| {
                this.remotes.clear();
                async {}
            }),
        ];
        let tick = cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            if this.update(cx, |_, cx| cx.notify()).is_err() {
                break;
            }
        });
        let mut this = Self {
            monitor,
            store,
            remotes: Vec::new(),
            selected: None,
            selected_reading: None,
            show_form: false,
            name,
            destination,
            error,
            _subscriptions: subscriptions,
            _tick: tick,
        };
        this.sync_hosts(cx);
        this
    }

    fn sync_hosts(&mut self, cx: &mut Context<Self>) {
        let mut previous = std::mem::take(&mut self.remotes);
        self.remotes = self
            .store
            .hosts()
            .iter()
            .map(|host| {
                if let Some(index) = previous
                    .iter()
                    .position(|r| r.host.destination == host.destination)
                {
                    let mut remote = previous.swap_remove(index);
                    remote.host = host.clone();
                    remote
                } else {
                    Remote {
                        host: host.clone(),
                        connection: Connection::new(
                            host.destination.clone(),
                            cx.background_executor(),
                        ),
                    }
                }
            })
            .collect();
        if !self
            .remotes
            .iter()
            .any(|remote| Some(&remote.host.destination) == self.selected.as_ref())
        {
            self.selected = self
                .remotes
                .first()
                .map(|remote| remote.host.destination.clone());
            self.selected_reading = None;
        }
    }

    fn add_host(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let host = Host::new(
            &self.name.read(cx).value(),
            &self.destination.read(cx).value(),
        );
        let result = host.and_then(|host| {
            let destination = host.destination.clone();
            self.store.add(host)?;
            self.selected = Some(destination);
            self.selected_reading = None;
            self.show_form = false;
            self.name
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.destination
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.sync_hosts(cx);
            Ok(())
        });
        self.error = result.err();
        cx.notify();
    }

    fn remove_host(&mut self, destination: &str, cx: &mut Context<Self>) {
        self.error = self.store.remove(destination).err();
        if self.error.is_none() {
            self.sync_hosts(cx);
        }
        cx.notify();
    }

    fn form(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_2()
            .p_3()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(theme::palette(cx).card)
            .child(
                h_flex()
                    .items_end()
                    .gap_3()
                    .child(v_flex().w(px(230.)).gap_1().child("Display name").child(Input::new(&self.name)))
                    .child(v_flex().flex_1().gap_1().child("SSH destination").child(Input::new(&self.destination)))
                    .child(
                        Button::new("save-host")
                            .primary()
                            .label("Save and connect")
                            .on_click(cx.listener(|this, _, window, cx| this.add_host(window, cx))),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Use a destination you already trust in OpenSSH. The remote machine needs zentop-cli on its SSH command PATH."),
            )
    }

    fn overview(&self, rows: &[Readings], cx: &mut Context<Self>) -> impl IntoElement {
        let monitor = self.monitor.read(cx);
        let metrics = monitor
            .frame
            .as_ref()
            .map(|frame| Metrics::from(&frame.stats));
        let age = monitor.frame.as_ref().map(|frame| {
            Duration::from_millis(sampling::now_ms().saturating_sub(frame.captured_at))
        });
        let (state, local_live) = if monitor.paused {
            ("Paused", false)
        } else if monitor.error.is_some() {
            ("Unavailable", false)
        } else {
            match age {
                Some(age)
                    if age
                        <= Duration::from_millis(
                            10_000.max(sampling::interval(monitor.refresh_rate()) * 3),
                        ) =>
                {
                    ("Live", true)
                }
                Some(_) => ("Waiting for update", false),
                None => ("Waiting for reading", false),
            }
        };

        let header = h_flex()
            .gap_3()
            .px_3()
            .py_2()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(div().flex_1().child("Host"))
            .children(
                ["CPU", "RAM", "Disk", "Uptime", "Status"]
                    .into_iter()
                    .zip(WIDTHS)
                    .map(|(label, width)| div().w(px(width)).flex_shrink_0().child(label)),
            );
        let local = h_flex()
            .gap_2()
            .flex_1()
            .min_w_0()
            .child(Icon::new(IconName::Monitor).small())
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Local"));
        let mut list = v_flex().child(header).child(host_row(
            local,
            metrics.as_ref(),
            state,
            local_live,
            age,
            false,
            cx,
        ));
        for (index, (remote, readings)) in self.remotes.iter().zip(rows).enumerate() {
            let latest = readings.recent.front();
            let age = latest.map(|reading| reading.received.elapsed());
            let live = readings.phase == Phase::Live
                && age.is_some_and(|age| age < Duration::from_secs(7));
            let state = match readings.phase {
                Phase::Connecting => "Connecting",
                Phase::Live if live => "Live",
                Phase::Live => "Waiting for update",
                Phase::Reconnecting { .. } => "Reconnecting",
                Phase::Disconnected => "Disconnected",
            };
            let destination = remote.host.destination.clone();
            let selected = self.selected.as_ref() == Some(&destination);
            let title = v_flex()
                .flex_1()
                .min_w_0()
                .items_start()
                .gap_0p5()
                .child(
                    Button::new(("select-ssh-host", index))
                        .small()
                        .ghost()
                        .label(remote.host.name.clone())
                        .tooltip(format!("View processes for {}", remote.host.name))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.selected = Some(destination.clone());
                            this.selected_reading = None;
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .px_2()
                        .text_xs()
                        .truncate()
                        .text_color(cx.theme().muted_foreground)
                        .child(remote.host.destination.clone()),
                );
            list = list.child(host_row(
                title,
                latest.map(|r| &r.sample.metrics),
                state,
                live,
                age,
                selected,
                cx,
            ));
        }
        list.rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(theme::palette(cx).card)
    }

    fn detail(
        &self,
        remote: &Remote,
        readings: &Readings,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let history = &readings.recent;
        let chosen = self
            .selected_reading
            .and_then(|received| history.iter().find(|r| r.received == received));
        let visible = chosen.or_else(|| history.front());
        let destination = remote.host.destination.clone();
        let remove = destination.clone();
        let action = |id, label, action| {
            let destination = destination.clone();
            Button::new(id)
                .small()
                .outline()
                .label(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(remote) = this
                        .remotes
                        .iter()
                        .find(|r| r.host.destination == destination)
                    {
                        remote.connection.send(action);
                        cx.notify();
                    }
                }))
        };
        let title = h_flex()
            .gap_2()
            .justify_between()
            .child(
                v_flex()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(remote.host.name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Recent readings"),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_shrink_0()
                    .when(
                        matches!(readings.phase, Phase::Reconnecting { .. }),
                        |row| row.child(action("retry-host", "Retry now", Action::Connect)),
                    )
                    .child(if readings.phase == Phase::Disconnected {
                        action("connect-host", "Connect", Action::Connect)
                    } else {
                        action("disconnect-host", "Disconnect", Action::Disconnect)
                    })
                    .child(
                        Button::new("remove-host")
                            .small()
                            .ghost()
                            .label("Remove host")
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.remove_host(&remove, cx)),
                            ),
                    ),
            );
        let mut detail = v_flex().gap_3().child(title);
        if let Phase::Reconnecting { error, at } = &readings.phase {
            let wait = at
                .saturating_duration_since(Instant::now())
                .as_secs_f32()
                .ceil() as u64;
            detail = detail.child(
                div()
                    .p_2()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().secondary)
                    .text_sm()
                    .child(format!("Retrying in {wait}s · {error}")),
            );
        }
        let Some(reading) = visible else {
            return detail.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("No process readings received yet."),
            );
        };
        let latest = Button::new("latest-reading")
            .small()
            .label("Latest")
            .when(chosen.is_none(), Button::primary)
            .when(chosen.is_some(), Button::outline)
            .on_click(cx.listener(|this, _, _, cx| {
                this.selected_reading = None;
                cx.notify();
            }));
        let mut timeline = h_flex().flex_wrap().gap_2().child(latest);
        for (index, sample) in history.iter().enumerate() {
            // A chosen reading stays selected as it moves through the history.
            if index == 0 && chosen.is_none_or(|r| r.received != sample.received) {
                continue;
            }
            let received = sample.received;
            let selected = chosen.is_some_and(|r| r.received == received);
            timeline = timeline.child(
                Button::new(("ssh-reading", index))
                    .small()
                    .label(age_label(received.elapsed()))
                    .when(selected, Button::primary)
                    .when(!selected, Button::outline)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_reading = Some(received);
                        cx.notify();
                    })),
            );
        }
        detail
            .child(timeline)
            .child(docker_panel(reading, cx))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("Top {MAX_PROCESSES} processes by CPU")),
            )
            .child(process_table(reading, cx))
    }
}

impl Render for SshHosts {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows: Vec<_> = self
            .remotes
            .iter()
            .map(|remote| remote.connection.read())
            .collect();
        let selected = self
            .remotes
            .iter()
            .zip(&rows)
            .find(|(remote, _)| Some(&remote.host.destination) == self.selected.as_ref());
        if self.selected_reading.is_some_and(|received| {
            selected
                .is_none_or(|(_, readings)| !readings.recent.iter().any(|r| r.received == received))
        }) {
            self.selected_reading = None;
        }
        let title = h_flex()
            .justify_between()
            .gap_4()
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("SSH Hosts"),
            )
            .child(
                Button::new("toggle-add-host")
                    .outline()
                    .label(if self.show_form { "Cancel" } else { "Add host" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_form = !this.show_form;
                        cx.notify();
                    })),
            );
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                TitleBar::new()
                    .bg(theme::palette(cx).card)
                    .border_b_1()
                    .border_color(cx.theme().border),
            )
            .child(
                v_flex()
                    .id("ssh-hosts-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .p_4()
                    .gap_4()
                    .child(title)
                    .when(self.show_form, |view| view.child(self.form(cx)))
                    .when_some(self.error.clone(), |view, error| {
                        view.child(
                            div()
                                .text_sm()
                                .text_color(theme::palette(cx).danger)
                                .child(error),
                        )
                    })
                    .child(self.overview(&rows, cx))
                    .when_some(selected, |view, (remote, readings)| {
                        view.child(self.detail(remote, readings, cx))
                    })
                    .when(self.remotes.is_empty() && !self.show_form, |view| {
                        view.child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(
                                "Add an OpenSSH destination to see its readings alongside Local.",
                            ),
                        )
                    }),
            )
    }
}

fn age_label(age: Duration) -> String {
    let seconds = age.as_secs();
    match seconds {
        0..=59 => format!("{seconds}s ago"),
        60..=3599 => format!("{}m ago", seconds / 60),
        3600..=86399 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86400),
    }
}

fn percent(used: u64, total: u64) -> Option<f64> {
    (total > 0).then(|| used as f64 / total as f64 * 100.)
}

fn capacity(used: u64, total: u64) -> String {
    if total == 0 {
        "—".into()
    } else {
        format!("{} / {}", format_bytes(used), format_bytes(total))
    }
}

fn meter(
    label: String,
    value: Option<f64>,
    index: usize,
    live: bool,
    cx: &App,
) -> impl IntoElement {
    let palette = theme::palette(cx);
    let color = if live {
        [palette.charts[0], palette.charts[1], palette.charts[3]][index]
    } else {
        cx.theme().muted_foreground
    };
    v_flex()
        .w(px(WIDTHS[index]))
        .flex_shrink_0()
        .gap_1()
        .child(
            div()
                .text_xs()
                .font_family(cx.theme().mono_font_family.clone())
                .child(label),
        )
        .child(
            div()
                .w_full()
                .h(px(3.))
                .rounded_full()
                .overflow_hidden()
                .bg(cx.theme().border)
                .when_some(value, |track, value| {
                    track.child(
                        div()
                            .h_full()
                            .w(relative(value.clamp(0., 100.) as f32 / 100.))
                            .bg(color),
                    )
                }),
        )
}

fn host_row(
    title: impl IntoElement,
    metrics: Option<&Metrics>,
    phase: &str,
    live: bool,
    age: Option<Duration>,
    selected: bool,
    cx: &App,
) -> impl IntoElement {
    let cpu = metrics.and_then(|m| m.cpu_percent);
    let dash = || "—".into();
    h_flex()
        .gap_3()
        .px_3()
        .py_2()
        .min_h(px(54.))
        .border_t_1()
        .border_color(cx.theme().border)
        .when(selected, |row| row.bg(cx.theme().secondary))
        .child(title)
        .child(meter(
            cpu.map_or_else(dash, |v| format!("{v:.0}%")),
            cpu,
            0,
            live,
            cx,
        ))
        .child(meter(
            metrics.map_or_else(dash, |m| capacity(m.memory_used, m.memory_total)),
            metrics.and_then(|m| percent(m.memory_used, m.memory_total)),
            1,
            live,
            cx,
        ))
        .child(meter(
            metrics.map_or_else(dash, |m| capacity(m.disk_used, m.disk_total)),
            metrics.and_then(|m| percent(m.disk_used, m.disk_total)),
            2,
            live,
            cx,
        ))
        .child(
            div()
                .w(px(WIDTHS[3]))
                .flex_shrink_0()
                .text_xs()
                .child(metrics.map_or_else(dash, |m| format_uptime(m.uptime_seconds))),
        )
        .child(
            v_flex()
                .w(px(WIDTHS[4]))
                .flex_shrink_0()
                .gap_0p5()
                .child(
                    h_flex()
                        .gap_2()
                        .text_xs()
                        .child(div().size_1p5().rounded_full().bg(if live {
                            theme::palette(cx).running
                        } else {
                            cx.theme().muted_foreground
                        }))
                        .child(phase.to_string()),
                )
                .when_some(age, |cell, age| {
                    cell.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("Received {}", age_label(age))),
                    )
                }),
        )
}

fn docker_panel(reading: &Reading, cx: &App) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;
    let palette = theme::palette(cx);
    let mut panel = v_flex()
        .gap_2()
        .p_3()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .bg(palette.card)
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child("Docker containers"),
        );
    let note = |message| div().text_xs().text_color(muted).child(message);
    match &reading.sample.docker {
        None => panel.child(note("Update zentop-cli on this host to see container health.")),
        Some(Docker::Unavailable) => panel.child(note("Docker status unavailable. Zentop needs the Docker CLI and access to the Linux host’s local Docker socket.")),
        Some(Docker::Available { containers, .. }) if containers.is_empty() => panel.child(note("No containers on this host.")),
        Some(Docker::Available { containers, truncated }) => {
            let mut rows: Vec<_> = containers.iter().collect();
            rows.sort_by_key(|container| {
                let priority = match (container.health, container.state) {
                    (Some(ContainerHealth::Unhealthy), _) | (_, ContainerState::Dead) => 0,
                    (_, ContainerState::Restarting | ContainerState::Exited | ContainerState::Paused) => 1,
                    _ => 2,
                };
                (priority, container.name.as_str())
            });
            if *truncated {
                panel = panel.child(note("Showing the 64 newest containers."));
            }
            panel.child(v_flex().id("ssh-docker-containers").max_h(px(220.)).overflow_y_scroll()
                .children(rows.into_iter().map(|container| {
                    let unhealthy = container.health == Some(ContainerHealth::Unhealthy);
                    h_flex().gap_3().py_1().border_t_1().border_color(cx.theme().border).text_xs()
                        .child(div().min_w_0().flex_1().truncate().child(container.name.clone()))
                        .child(div().w(px(100.)).flex_shrink_0().child(container.state.label()))
                        .child(div().w(px(110.)).flex_shrink_0().text_color(if unhealthy { palette.danger } else { muted })
                            .child(container.health.map_or("—", ContainerHealth::label)))
                })))
        }
    }
}

fn process_table(reading: &Reading, cx: &App) -> impl IntoElement {
    let row = |pid: String, name: String, cpu: String, ram: String| {
        h_flex()
            .gap_3()
            .px_3()
            .py_1()
            .text_xs()
            .child(div().w(px(80.)).child(pid))
            .child(div().min_w_0().flex_1().truncate().child(name))
            .child(div().w(px(100.)).child(cpu))
            .child(div().w(px(120.)).child(ram))
    };
    let mut table = v_flex()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .bg(theme::palette(cx).card)
        .child(
            row("PID".into(), "Process".into(), "CPU".into(), "RAM".into())
                .text_color(cx.theme().muted_foreground),
        );
    for process in &reading.sample.processes {
        table = table.child(
            row(
                process.pid.to_string(),
                if process.name.is_empty() {
                    "—".into()
                } else {
                    process.name.clone()
                },
                format!("{:.1}%", process.cpu_percent),
                format_bytes(process.memory_bytes),
            )
            .border_t_1()
            .border_color(cx.theme().border),
        );
    }
    table.when(reading.sample.processes.is_empty(), |table| {
        table.child(
            div()
                .p_3()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("No accessible processes in this reading."),
        )
    })
}
