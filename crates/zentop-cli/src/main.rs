//! One-shot system and process reports backed by the shared sampler.
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
    /// Show named hardware temperature sensors in Celsius
    Temperature,
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
    let snapshot = capture()?;
    match args.command.unwrap_or(Command::System) {
        Command::System => {
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

// Process names are OS data and may contain terminal escape sequences.
fn plain(value: &str) -> String {
    value.chars().filter(|c| !c.is_control()).collect()
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
