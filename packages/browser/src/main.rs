//! `tick-browser`: the Browser Tick, and the native messaging host Chrome starts for the extension.

use std::{path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand};
use tick_browser::{catalog::Catalog, native_host, registration, socket, tick};
use tick_command::{
    cli::{CONFIGURATION_HELP, GlobalArgs},
    entry,
};
use tick_provider::error::{Error, Result};

const ABOUT: &str =
    "The Agent's tabs in your own Chrome, Chromium or Edge, through the Tickloop browser extension";

const EXAMPLES: &str = "Examples:
  tick-browser browser
  tick-browser status

Chrome starts `tick-browser chrome-extension://<id>/` itself as the native messaging host
ai.tickloop.browser, which the Browser Tick registers when it opens. Each session opens its own
Browser Tick, and they all use the one browser that serves.";

const EXIT_CODES: &str = "Exit codes:
  0  success
  2  usage or configuration error";

#[derive(Parser)]
#[command(
    name = "tick-browser",
    version,
    about = ABOUT,
    after_help = format!("{EXAMPLES}\n\n{CONFIGURATION_HELP}\n\n{EXIT_CODES}")
)]
struct Cli {
    #[command(flatten)]
    global: GlobalArgs,
    /// The directory of the local socket the native host serves; for tests
    #[arg(long, global = true, hide = true, value_name = "DIR")]
    socket_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show the Agent's tabs and serve the browser extension's tools
    Browser,
    /// Print where the Tick connects, whether a browser serves it and how the native messaging host is registered, as JSON
    Status,
    /// Print the Tick descriptor as JSON, as it stands before the extension connects
    Describe,
}

fn main() -> ExitCode {
    if let Some(launch) = native_host::launched_by_browser(std::env::args_os().skip(1)) {
        return native_host::main(launch);
    }
    entry::run(execute)
}

/// The directory of the local socket: the one a test names, or the user's own.
fn socket_directory(named: Option<PathBuf>) -> Result<PathBuf> {
    match named {
        Some(directory) => Ok(directory),
        None => socket::default_directory()
            .map_err(|error| Error::usage(format!("cannot find the browser socket: {error}"))),
    }
}

async fn execute(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Describe => {
            entry::refuse_in_instance()?;
            print_json(&[Catalog::default().descriptor()])
        }
        Command::Status => {
            entry::refuse_in_instance()?;
            let registration = match (std::env::home_dir(), registration::current_executable()) {
                (Some(home), Ok(executable)) => Ok(registration::inspect(&home, &executable)),
                (None, _) => Err("this user has no home directory".to_owned()),
                (_, Err(error)) => Err(format!("cannot find this program's path: {error}")),
            };
            let socket_directory = socket_directory(cli.socket_dir)?;
            let socket = socket_directory.join(socket::SOCKET_NAME);
            let mut status = tick::status_report(&tick::Setup {
                session: tick::Session::current(),
                socket: socket.clone(),
                registration,
            });
            status["host_running"] = socket::is_served(&socket_directory).into();
            print_json(&status)
        }
        Command::Browser => {
            entry::check_locale()?;
            let settings = cli.global.settings()?;
            let socket_directory = socket_directory(cli.socket_dir)?;
            tick::run(&settings, cli.global.size, socket_directory).await
        }
    }
}

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    serde_json::to_writer_pretty(std::io::stdout(), value)
        .map_err(|error| Error::usage(error.to_string()))?;
    println!();
    Ok(())
}
