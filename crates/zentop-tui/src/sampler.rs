//! Keep sampling off the event loop; commands wake the sampler without polling.
use std::{
    io,
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use zentop_core::{
    sampling::{Hub, Snapshot},
    state::AppState,
};

pub enum Command {
    Pause(bool),
    Interval(u64),
    Demand { gpu: bool, ports: bool },
    Stop,
}

pub struct Sampler {
    pub readings: Receiver<Result<Snapshot, String>>,
    commands: mpsc::Sender<Command>,
    worker: Option<JoinHandle<()>>,
}

impl Sampler {
    pub fn start(interval: u64) -> io::Result<Self> {
        let (readings_tx, readings) = mpsc::sync_channel(1);
        let (commands, control) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("zentop-sampler".into())
            .spawn(move || {
                let state = AppState::new();
                let mut hub = Hub::default();
                hub.interval = interval;
                let mut ports = false;
                let mut due = Instant::now();
                loop {
                    if Instant::now() >= due {
                        let mut result = hub.read(&state);
                        if ports {
                            if let Ok(snapshot) = &mut result {
                                snapshot.attach_ports();
                            }
                        }
                        if let Err(mpsc::TrySendError::Disconnected(_)) =
                            readings_tx.try_send(result)
                        {
                            break;
                        }
                        due = Instant::now() + Duration::from_millis(hub.interval);
                    }
                    match control.recv_timeout(due.saturating_duration_since(Instant::now())) {
                        Ok(Command::Pause(paused)) => {
                            hub.pause(paused);
                            due = Instant::now();
                        }
                        Ok(Command::Interval(interval)) => {
                            hub.interval = interval;
                            due = Instant::now();
                        }
                        Ok(Command::Demand {
                            gpu,
                            ports: requested,
                        }) => {
                            hub.process_gpu = gpu;
                            ports = requested;
                            due = Instant::now();
                        }
                        Ok(Command::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                        Err(RecvTimeoutError::Timeout) => {}
                    }
                }
            })?;
        Ok(Self {
            readings,
            commands,
            worker: Some(worker),
        })
    }

    pub fn send(&self, command: Command) -> io::Result<()> {
        self.commands
            .send(command)
            .map_err(|_| io::Error::other("Sampling worker stopped"))
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
