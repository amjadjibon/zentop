//! One-shot system and process reports backed by the shared sampler.
#[cfg(target_os = "linux")]
mod docker;
#[cfg(target_os = "macos")]
mod traffic;

use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;
use std::{
    io::{self, Write},
    process::ExitCode,
    thread,
    time::Duration,
};
use zentop_core::{
    monitoring::ProcessInfo,
    sampling::{Hub, Snapshot},
    ssh_stream,
    state::AppState,
};

#[derive(Debug, Parser)]
#[command(version, about = "Inspect system resources and processes")]
struct Args {
    /// Output JSON instead of a human-readable report
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show system resources (the default command)
    System,
    /// Stream bounded JSON Lines readings for SSH Hosts
    Stream {
        /// Wire protocol version
        #[arg(long, value_parser = clap::value_parser!(u32).range(i64::from(ssh_stream::VERSION)..=i64::from(ssh_stream::DOCKER_VERSION)))]
        protocol: u32,
        /// End the stream after this many readings
        #[arg(long, default_value_t = ssh_stream::MAX_SAMPLES, value_parser = clap::value_parser!(u32).range(1..=i64::from(ssh_stream::MAX_SAMPLES)))]
        samples: u32,
    },
    /// Show named hardware temperature sensors in Celsius
    Temperature,
    /// Inspect one process's TCP connections and bound UDP sockets
    Connections {
        #[arg(value_parser = clap::value_parser!(u32).range(1..))]
        pid: u32,
    },
    /// Inspect manually routed HTTP traffic and HTTPS tunnels
    #[cfg(target_os = "macos")]
    Traffic {
        /// Fixed loopback port (defaults to an OS-assigned port)
        #[arg(long, value_parser = clap::value_parser!(u16).range(1..))]
        port: Option<u16>,
        /// Include redacted HTTP headers
        #[arg(long)]
        headers: bool,
    },
    /// List processes, busiest first
    Processes {
        /// Maximum number of processes to show
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=10000))]
        limit: u32,
        #[arg(long, value_enum, default_value_t = Sort::Cpu)]
        sort: Sort,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Sort {
    Cpu,
    Memory,
    Pid,
}

// Export only the columns shown in the report, rather than process environments.
#[derive(Serialize)]
struct ProcessRow<'a> {
    pid: u32,
    name: &'a str,
    cpu_percent: f32,
    memory_bytes: u64,
    status: &'a str,
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(args, &mut io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("zentop-cli: {error}");
            ExitCode::FAILURE
        }
    }
}

fn capture() -> io::Result<Snapshot> {
    let state = AppState::new();
    let mut hub = Hub::default();
    hub.interval = 1500;
    // CPU and network rates need a previous observation and elapsed time.
    hub.read(&state).map_err(io::Error::other)?;
    thread::sleep(Duration::from_millis(hub.interval));
    let snapshot = hub.read(&state).map_err(io::Error::other)?;
    if let Some(error) = &snapshot.error {
        return Err(io::Error::other(error.clone()));
    }
    Ok(snapshot)
}

fn run(args: Args, output: &mut impl Write) -> io::Result<()> {
    match args.command.unwrap_or(Command::System) {
        #[cfg(target_os = "macos")]
        Command::Traffic { port, headers } => traffic::run(port, args.json, headers, output)?,
        Command::Connections { pid } => connection_report(pid, args.json, output)?,
        Command::Stream { protocol, samples } => stream(protocol, samples, output)?,
        Command::System => {
            let snapshot = capture()?;
            let stats = &snapshot.frame.stats;
            if args.json {
                serde_json::to_writer_pretty(&mut *output, stats)?;
                writeln!(output)?;
            } else {
                let cpu = snapshot
                    .history
                    .last()
                    .and_then(|s| s.cpu)
                    .map_or_else(|| "Unavailable".into(), |v| format!("{v:.1}%"));
                writeln!(output, "CPU: {cpu}")?;
                writeln!(
                    output,
                    "Memory: {:.1} / {:.1} GiB",
                    stats.memory_used as f64 / 1073741824.,
                    stats.memory_total as f64 / 1073741824.
                )?;
                writeln!(output, "Uptime: {}s", stats.uptime)?;
                writeln!(output, "Processes: {}", snapshot.frame.processes.len())?;
            }
        }
        Command::Temperature => {
            let snapshot = capture()?;
            let sensors = &snapshot.frame.stats.temperatures;
            if args.json {
                serde_json::to_writer_pretty(&mut *output, sensors)?;
                writeln!(output)?;
            } else {
                for sensor in sensors {
                    writeln!(
                        output,
                        "{}: {} · min {} · max {}",
                        plain(&sensor.name),
                        zentop_core::temperature::format(sensor.celsius),
                        zentop_core::temperature::format(sensor.minimum),
                        zentop_core::temperature::format(sensor.maximum)
                    )?;
                }
            }
        }
        Command::Processes { limit, sort } => {
            let snapshot = capture()?;
            let mut processes: Vec<&ProcessInfo> = snapshot.frame.processes.iter().collect();
            processes.sort_by(|a, b| match sort {
                Sort::Cpu => b.cpu_usage.total_cmp(&a.cpu_usage).then(a.pid.cmp(&b.pid)),
                Sort::Memory => b.memory_usage.cmp(&a.memory_usage).then(a.pid.cmp(&b.pid)),
                Sort::Pid => a.pid.cmp(&b.pid),
            });
            let rows: Vec<_> = processes
                .into_iter()
                .take(limit as usize)
                .map(|p| ProcessRow {
                    pid: p.pid,
                    name: &p.name,
                    cpu_percent: p.cpu_usage,
                    memory_bytes: p.memory_usage,
                    status: &p.status,
                })
                .collect();
            if args.json {
                serde_json::to_writer_pretty(&mut *output, &rows)?;
                writeln!(output)?;
            } else {
                writeln!(
                    output,
                    "{:>7} {:>7} {:>11} {:<12} NAME",
                    "PID", "CPU %", "RAM MiB", "STATUS"
                )?;
                for row in rows {
                    writeln!(
                        output,
                        "{:>7} {:>7.1} {:>11.1} {:<12} {}",
                        row.pid,
                        row.cpu_percent,
                        row.memory_bytes as f64 / 1048576.,
                        plain(row.status),
                        plain(row.name)
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn stream(protocol: u32, samples: u32, output: &mut impl Write) -> io::Result<()> {
    let mut docker = None;
    let state = AppState::new();
    let mut hub = Hub::default();
    hub.interval = ssh_stream::INTERVAL.as_millis() as u64;
    // Prime the shared collector before sending CPU readings.
    hub.read(&state).map_err(io::Error::other)?;
    for sequence in 1..=samples {
        thread::sleep(ssh_stream::INTERVAL);
        let snapshot = hub.read(&state).map_err(io::Error::other)?;
        if let Some(error) = snapshot.error {
            return Err(io::Error::other(error));
        }
        let mut sample = ssh_stream::Sample::from_frame(sequence, &snapshot.frame);
        if protocol == ssh_stream::DOCKER_VERSION {
            if (sequence - 1).is_multiple_of(5) {
                docker = Some(docker_health());
            }
            sample.version = protocol;
            sample.docker.clone_from(&docker);
        }
        let mut line = serde_json::to_vec(&sample)?;
        line.push(b'\n');
        if line.len() > ssh_stream::MAX_LINE_BYTES {
            return Err(io::Error::other("SSH reading exceeded the line budget"));
        }
        output.write_all(&line)?;
        output.flush()?;
    }
    Ok(())
}

fn docker_health() -> ssh_stream::Docker {
    #[cfg(target_os = "linux")]
    {
        docker::collect()
    }
    #[cfg(not(target_os = "linux"))]
    {
        ssh_stream::Docker::Unavailable
    }
}

// Process names are OS data and may contain terminal escape sequences.
fn plain(value: &str) -> String {
    value.chars().filter(|c| !c.is_control()).collect()
}

fn connection_report(pid: u32, json: bool, output: &mut impl Write) -> io::Result<()> {
    let start = zentop_core::connections::process_start_time(pid).map_err(io::Error::other)?;
    let report = zentop_core::connections::collect(pid, start).map_err(io::Error::other)?;
    if json {
        serde_json::to_writer_pretty(&mut *output, &report)?;
        writeln!(output)?;
    } else {
        writeln!(
            output,
            "PID {pid} · {} sockets · snapshot {}",
            report.connections.len(),
            report.captured_at
        )?;
        for c in &report.connections {
            writeln!(
                output,
                "{} {:<12} {} → {}",
                c.protocol.label(),
                c.state,
                c.local,
                c.remote_label()
            )?;
        }
        if report.connections.is_empty() {
            writeln!(output, "No accessible connections reported.")?;
        }
        if report.truncated {
            writeln!(output, "Showing the first 512 sockets; more were reported.")?;
        }
        if report.partial {
            writeln!(
                output,
                "Some sockets could not be read; results are partial."
            )?;
        }
        writeln!(
            output,
            "Permissions may hide sockets. UDP peer addresses are unavailable."
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_process_limits() {
        for value in ["0", "10001", "invalid"] {
            assert!(Args::try_parse_from(["zentop-cli", "processes", "--limit", value]).is_err());
        }
    }

    #[test]
    fn stream_rejects_unsupported_versions_and_sample_counts() {
        for (option, value) in [
            ("--protocol", "3"),
            ("--samples", "0"),
            ("--samples", "1201"),
        ] {
            let mut args = vec!["zentop-cli", "stream"];
            if option != "--protocol" {
                args.extend(["--protocol", "1"]);
            }
            args.extend([option, value]);
            assert!(Args::try_parse_from(args).is_err());
        }
    }

    #[test]
    fn json_flag_works_after_subcommand() {
        let args = Args::try_parse_from(["zentop-cli", "processes", "--json", "--sort", "memory"])
            .unwrap();
        assert!(args.json);
        assert!(matches!(
            args.command,
            Some(Command::Processes {
                sort: Sort::Memory,
                ..
            })
        ));
    }

    #[test]
    fn text_output_removes_terminal_controls() {
        assert_eq!(plain("server\x1b[2J\n"), "server[2J");
    }
}
