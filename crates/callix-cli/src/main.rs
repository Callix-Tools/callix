//! `callix` — analyse a project and hand back the graph, or a report from it.
//!
//! The command line is a thin shell: language adapters and their resolvers live
//! in the `callix` crate and run inside an embedded interpreter (`bridge`);
//! everything that turns a graph into a file format lives in `callix-export`
//! and never touches Python.

mod bridge;
mod cli;
mod commands;
mod failure;
mod input;
mod prepare;

use std::process::ExitCode;

use clap::Parser;

fn main() -> ExitCode {
    let cli = cli::Cli::parse();
    match commands::run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            if !failure.message.is_empty() {
                eprintln!("error: {}", failure.message);
            }
            ExitCode::from(failure.code)
        }
    }
}
