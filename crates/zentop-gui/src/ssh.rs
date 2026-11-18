//! Cancellable OpenSSH sessions for explicitly configured destinations.
use gpui_kit::{BackgroundExecutor, Task};
use smol::{
    channel::{self, Receiver, Sender},
    future::{or, race, yield_now},
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, BufReader},
    process::Command,
    Timer,
};
use std::{
    collections::VecDeque,
    ffi::OsStr,
    future::pending,
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use zentop_core::ssh_stream::{self, Sample};

const HISTORY: usize = 8;
const FIRST_SAMPLE_TIMEOUT: Duration = Duration::from_secs(20);
const SAMPLE_TIMEOUT: Duration = Duration::from_secs(12);
const SSH_OPTIONS: &[&str] = &[
    "-T",
    "-n",
    "-a",
    "-x",
    "-oBatchMode=yes",
    "-oStrictHostKeyChecking=yes",
    "-oUpdateHostKeys=no",
    "-oClearAllForwardings=yes",
    "-oControlMaster=no",
    "-oControlPath=none",
    "-oPermitLocalCommand=no",
    "-oRemoteCommand=none",
    "-oIgnoreUnknown=ForkAfterAuthentication",
    "-oForkAfterAuthentication=no",
    "-oConnectTimeout=8",
    "-oServerAliveInterval=5",
    "-oServerAliveCountMax=2",
    "--",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Connecting,
    Live,
    Reconnecting { error: String, at: Instant },
    Disconnected,
}

#[derive(Clone)]
pub struct Reading {
    pub received: Instant,
    pub sample: Arc<Sample>,
}

#[derive(Clone)]
pub struct Readings {
    pub phase: Phase,
    pub recent: VecDeque<Reading>,
}

impl Readings {
    fn record(&mut self, sample: Sample) {
        if self.recent.len() == HISTORY {
            self.recent.pop_back();
        }
        self.recent.push_front(Reading {
            received: Instant::now(),
            sample: Arc::new(sample),
        });
        self.phase = Phase::Live;
    }
}

type Shared = Arc<Mutex<Readings>>;

#[derive(Clone, Copy)]
pub enum Action {
    Connect,
    Disconnect,
}

pub struct Connection {
    actions: Sender<Action>,
    state: Shared,
    _worker: Task<()>,
}

impl Connection {
    pub fn new(destination: String, executor: &BackgroundExecutor) -> Self {
        let state = Arc::new(Mutex::new(Readings {
            phase: Phase::Connecting,
            recent: VecDeque::new(),
        }));
        let (actions, receiver) = channel::bounded(4);
        let worker_state = state.clone();
        let worker = executor.spawn(async move {
            supervise("ssh".as_ref(), &destination, &worker_state, receiver).await;
        });
        Self {
            actions,
            state,
            _worker: worker,
        }
    }

    pub fn read(&self) -> Readings {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn send(&self, action: Action) {
        let _ = self.actions.try_send(action);
    }
}

fn report(state: &Shared, phase: Phase) {
    state.lock().unwrap_or_else(|e| e.into_inner()).phase = phase;
}

enum End {
    Finished(Result<(), String>),
    Stop,
}

async fn supervise(program: &OsStr, destination: &str, state: &Shared, controls: Receiver<Action>) {
    let mut failures = 0;
    loop {
        report(state, Phase::Connecting);
        let mut accepted = 0;
        let end = race(
            async {
                End::Finished(
                    read_session(program, destination, |sample| {
                        accepted += 1;
                        state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .record(sample);
                    })
                    .await,
                )
            },
            async {
                // Ignore repeated Connect clicks while a session is starting.
                while let Ok(Action::Connect) = controls.recv().await {}
                End::Stop
            },
        )
        .await;

        let action = match end {
            End::Stop => Some(Action::Disconnect),
            End::Finished(result) => {
                if accepted >= 3 {
                    failures = 0;
                }
                let delay = match result {
                    Ok(()) => {
                        report(state, Phase::Connecting);
                        ssh_stream::INTERVAL
                    }
                    Err(error) => {
                        let delay = Duration::from_secs((2_u64 << failures).min(30));
                        failures = (failures + 1).min(4);
                        report(
                            state,
                            Phase::Reconnecting {
                                error,
                                at: Instant::now() + delay,
                            },
                        );
                        delay
                    }
                };
                race(async { controls.recv().await.ok() }, async {
                    Timer::after(delay).await;
                    Some(Action::Connect)
                })
                .await
            }
        };
        match action {
            Some(Action::Connect) => {}
            Some(Action::Disconnect) => {
                failures = 0;
                report(state, Phase::Disconnected);
                loop {
                    match controls.recv().await {
                        Ok(Action::Connect) => break,
                        Ok(Action::Disconnect) => {}
                        Err(_) => return,
                    }
                }
            }
            None => return,
        }
    }
}

fn ssh_command(program: &OsStr, destination: &str) -> Command {
    let mut command = Command::new(program);
    command
        .args(SSH_OPTIONS)
        .arg(destination)
        .arg(format!(
            "zentop-cli stream --protocol {}",
            ssh_stream::VERSION
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use smol::process::windows::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    command.kill_on_drop(true).reap_on_drop(true);
    command
}

async fn read_session(
    program: &OsStr,
    destination: &str,
    mut on_sample: impl FnMut(Sample),
) -> Result<(), String> {
    let mut child = ssh_command(program, destination)
        .spawn()
        .map_err(|e| format!("Could not start OpenSSH: {e}"))?;
    let mut stdout = BufReader::new(child.stdout.take().ok_or("OpenSSH stdout unavailable.")?);
    let mut stderr = child.stderr.take().ok_or("OpenSSH stderr unavailable.")?;
    let mut diagnostic = Vec::new();
    let result = or(
        async {
            // Drain stderr so a noisy remote cannot block stdout, and keep only
            // a small diagnostic for disconnects.
            let mut buffer = [0; 1024];
            while let Ok(count @ 1..) = stderr.read(&mut buffer).await {
                let keep = count.min(2048 - diagnostic.len());
                diagnostic.extend_from_slice(&buffer[..keep]);
                yield_now().await;
            }
            pending::<Result<(), String>>().await
        },
        async {
            let mut expected = 1;
            let mut line = Vec::new();
            loop {
                line.clear();
                let timeout = if expected == 1 {
                    FIRST_SAMPLE_TIMEOUT
                } else {
                    SAMPLE_TIMEOUT
                };
                let count = read_line(&mut stdout, &mut line, timeout).await?;
                if count == 0 {
                    let exited = race(async { child.status().await.ok() }, async {
                        Timer::after(Duration::from_millis(250)).await;
                        None
                    })
                    .await;
                    if expected > ssh_stream::MAX_SAMPLES
                        && exited.is_some_and(|status| status.success())
                    {
                        return Ok(());
                    }
                    return Err(exited.map_or_else(
                        || "SSH stream closed.".into(),
                        |status| {
                            if status.success() {
                                "SSH stream ended early.".into()
                            } else {
                                format!("SSH command failed ({status}).")
                            }
                        },
                    ));
                }
                let sample = ssh_stream::decode_sample(&line).map_err(|e| e.to_string())?;
                if sample.sequence != expected {
                    return Err("SSH readings arrived out of sequence.".into());
                }
                on_sample(sample);
                expected += 1;
                yield_now().await;
            }
        },
    )
    .await;
    result.map_err(|mut error| {
        let text = String::from_utf8_lossy(&diagnostic);
        let flat = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let detail = ssh_stream::safe_display(&flat, 240);
        if !detail.is_empty() {
            error.push(' ');
            error.push_str(&detail);
        }
        error
    })
}

async fn read_line(
    stdout: &mut (impl AsyncBufRead + Unpin),
    line: &mut Vec<u8>,
    timeout: Duration,
) -> Result<usize, String> {
    race(
        async {
            stdout
                .take((ssh_stream::MAX_LINE_BYTES + 1) as u64)
                .read_until(b'\n', line)
                .await
                .map_err(|e| format!("Could not read SSH stream: {e}"))
        },
        async {
            Timer::after(timeout).await;
            Err("Timed out waiting for an SSH reading.".into())
        },
    )
    .await
}

#[cfg(all(test, unix))]
mod tests;
