mod cgroups;
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
    /// CPU shares for the container, e.g. 0.5, 1, etc
    #[arg(short, long)]
    cpu: Option<String>,

    /// Memory limit for the container in bytes or Mb/Gb, e.g. 128M, 1Gb, etc
    #[arg(short, long)]
    mem: Option<String>,

    /// Command to execute in the container
    #[arg(required = true)]
    command: String,

    /// Arguments for the command
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

fn main() -> ExitCode {
    let args = Args::parse();

    if let Err(e) = run_in_container(&args.command, &args.args, &args.cpu, &args.mem) {
        eprintln!("Error: {:#}", e);
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
