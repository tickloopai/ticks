//! `tick-tickloop`: the official Tickloop Ticks, one subcommand each.

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use tick_command::{
    cli::{CONFIGURATION_HELP, GlobalArgs},
    entry::{self, Running},
};
use tick_market::{args::ReaderArgs, metadata::TickKind};
use tick_provider::error::Result;

const ABOUT: &str = "The official Tickloop Ticks as standalone terminal commands";

const EXAMPLES: &str = "Examples:
  tick-tickloop reader ./earnings-release.pdf
  tick-tickloop reader ./earnings-release.pdf --symbol AAPL.US --fiscal-period FY2026Q3
  tick-tickloop reader https://example.com/investors/results.html --once --format json";

const EXIT_CODES: &str = "Exit codes:
  0  success
  2  usage or configuration error
  4  network or API error";

#[derive(Parser)]
#[command(
    name = "tick-tickloop",
    version,
    about = ABOUT,
    after_help = format!("{EXAMPLES}\n\n{CONFIGURATION_HELP}\n\n{EXIT_CODES}")
)]
struct Cli {
    #[command(flatten)]
    global: GlobalArgs,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Read original financial documents from URLs or local files
    Reader(ReaderArgs),
    /// Print Tick descriptors and minimum Panel sizes as JSON
    Describe,
}

fn main() -> ExitCode {
    entry::run(execute)
}

async fn execute(cli: Cli) -> Result<()> {
    let args = match cli.command {
        Command::Reader(args) => args,
        Command::Describe => {
            entry::refuse_in_instance()?;
            return entry::describe(&[TickKind::Reader]);
        }
    };
    if args.output.once {
        entry::refuse_in_instance()?;
    }
    entry::check_locale()?;
    let settings = cli.global.settings()?;
    let context = tick_market::research::ResearchContext::from_environment()?;
    let library = if args.check || args.output.once {
        tick_market::reader::load(&args).await?
    } else {
        tick_market::reader::load_for_context(&args, context.as_ref()).await?
    };
    if args.check {
        return Ok(());
    }
    entry::run_tick(Running::Reader(args, library), &settings, cli.global.size).await
}
