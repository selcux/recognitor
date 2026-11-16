//! `recognitor` CLI: a thin layer over the library. Commands are defined in doc 07 §12.

use std::process::ExitCode;

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("--version" | "-V") => {
            println!("recognitor {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("usage: recognitor <command>\n\nNo commands are implemented yet.");
            ExitCode::from(2)
        }
    }
}
