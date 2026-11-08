//! Interactive terminal frontend backed by the shared monitoring crate.
mod app;
mod sampler;
mod theme;
mod ui;

use clap::Parser;
use std::{
    io::{self, IsTerminal},
    process::ExitCode,
};

#[derive(Debug, Parser)]
#[command(version, about = "Monitor CPU, memory and processes in your terminal")]
struct Args {
    /// Refresh interval in milliseconds
    #[arg(long, default_value_t = 3000, value_parser = clap::value_parser!(u64).range(1500..=30000))]
    interval: u64,
    /// Initial palette from the GUI theme collection
    #[arg(long, default_value = "catppuccin")]
    theme: String,
}

struct RestoreTerminal;
impl Drop for RestoreTerminal {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("zentop-tui: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> io::Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(io::Error::other(
            "an interactive terminal is required; use zentop-cli for reports",
        ));
    }
    let mut app = app::App::new(args.interval, &args.theme)?;
    // Also restore after partial initialization failures. Ratatui installs a panic hook.
    let _restore = RestoreTerminal;
    let mut terminal = ratatui::try_init()?;
    let sampler = sampler::Sampler::start(args.interval)?;
    app.run(&mut terminal, &sampler)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_matches_the_core_sampling_range() {
        for value in ["0", "1499", "30001", "invalid"] {
            assert!(Args::try_parse_from(["zentop-tui", "--interval", value]).is_err());
        }
        for value in ["1500", "3000", "30000"] {
            assert!(Args::try_parse_from(["zentop-tui", "--interval", value]).is_ok());
        }
    }
}

#[cfg(test)]
mod tests_ui;
