mod container;
mod fs;
mod net;

use std::process::ExitCode;

use clap::Parser;

use container::run_in_container;

/// A simple container runtime demonstrating Linux namespaces and cgroups
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Hostname for the container
    #[arg(long)]
    hostname: Option<String>,

    /// Drop all the capabilities for the command
    #[arg(long)]
    drop_caps: bool,

    /// Command to execute in the container
    #[arg(required = true)]
    command: String,

    /// Arguments for the command
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

fn main() -> ExitCode {
    let args = Args::parse();

    if let Err(e) = run_in_container(&args.command, &args.args, &args.hostname, args.drop_caps) {
        eprintln!("Error: {:#}", e);
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
