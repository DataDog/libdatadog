use std::{io, time::Duration};

use clap::{arg, command, Args, Parser, Subcommand};

fn parse_ms_duration(val: &str) -> io::Result<Duration> {
    let duration = Duration::from_millis(
        val.parse()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?,
    );
    Ok(duration)
}

#[derive(Args, Debug)]
pub struct StressParams {
    #[arg(short, long, default_value_t = std::cmp::max(1, num_cpus::get_physical()/2))]
    pub dl_open_threads: usize,

    #[arg(short, long, default_value_t = num_cpus::get_physical())]
    pub malloc_threads: usize,
}

#[derive(Args, Debug)]
pub struct ExecuteCmdArgs {
    pub command: String,

    #[command(flatten)]
    pub params: StressParams,

    #[arg(short, long, default_value_t = 1)]
    pub num_executions: usize,
    #[arg(short, long, default_value_t = num_cpus::get_physical())]
    pub parallel_executions: usize,

    #[arg(
        short,
        long,
        default_value = "1",
        value_parser(clap::builder::ValueParser::new(parse_ms_duration))
    )]
    pub warmup_time_ms: Duration,

    #[arg(raw(true))]
    pub command_args: Vec<String>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    #[command(name = "exec")]
    ExecuteCmd(ExecuteCmdArgs),
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}
