//! Terminal frontend scaffold. Monitoring is provided by `zentop-core`.
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => {
            println!("zentop-tui: frontend scaffold; monitoring commands are not implemented yet.");
            ExitCode::SUCCESS
        }
        [arg] if arg == "--help" || arg == "-h" => {
            println!("Usage: zentop-tui [--help | --version]\n\nTerminal frontend scaffold for Zentop, backed by zentop-core.");
            ExitCode::SUCCESS
        }
        [arg] if arg == "--version" || arg == "-V" => {
            println!("zentop-tui {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("Unsupported arguments. Run zentop-tui --help for usage.");
            ExitCode::FAILURE
        }
    }
}
