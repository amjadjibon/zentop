//! Command-line frontend scaffold. Monitoring is provided by `zentop-core`.
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => {
            println!("zentop-cli: frontend scaffold; monitoring commands are not implemented yet.");
            ExitCode::SUCCESS
        }
        [arg] if arg == "--help" || arg == "-h" => {
            println!("Usage: zentop-cli [--help | --version]\n\nCommand-line frontend scaffold for Zentop, backed by zentop-core.");
            ExitCode::SUCCESS
        }
        [arg] if arg == "--version" || arg == "-V" => {
            println!("zentop-cli {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("Unsupported arguments. Run zentop-cli --help for usage.");
            ExitCode::FAILURE
        }
    }
}
