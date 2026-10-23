//! `hyprgallery`: one binary for the daemon, the Waybar module and every
//! command that talks to them.

mod bar;
mod client;
mod daemon;
mod server;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use hyprgallery_core::Layout;
use hyprgallery_core::ipc::{Request, RotateRequest};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "hyprgallery", version, about = "A wallpaper gallery for Hyprland and Waybar.")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Open the gallery, or close it if it is open. The default command.
    Toggle {
        /// overlay (full screen) or popover (a panel under the bar).
        #[arg(long)]
        layout: Option<Layout>,
    },
    /// Open the gallery.
    Show {
        #[arg(long)]
        layout: Option<Layout>,
    },
    /// Close the gallery.
    Hide,
    /// Stream status as JSON lines, for a Waybar custom module.
    Bar,
    /// Set a wallpaper from a file.
    Set { path: PathBuf },
    /// The next wallpaper in an album (the current one's album by default).
    Next {
        #[arg(long)]
        album: Option<String>,
    },
    /// The previous wallpaper in an album.
    Prev {
        #[arg(long)]
        album: Option<String>,
    },
    /// A random wallpaper from an album.
    Random {
        #[arg(long)]
        album: Option<String>,
    },
    /// Change the wallpaper on a timer: `rotate 1h`, `rotate 15m --album Space
    /// --shuffle`, or `rotate off`.
    Rotate {
        /// An interval such as 15m, 1h, 1h30m, daily, or `off`.
        every: String,
        #[arg(long)]
        album: Option<String>,
        #[arg(long)]
        shuffle: bool,
    },
    /// Put the last wallpaper back. The daemon does this when it starts.
    Restore,
    /// Read the wallpaper folders again.
    Reindex,
    /// Show the current wallpaper and what the daemon is doing.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Run the gallery. Any other command starts it when it is not running.
    Daemon {
        /// Do not reapply the last wallpaper on start.
        #[arg(long)]
        no_restore: bool,
    },
    /// Stop the daemon.
    Quit,
    /// Render the gallery to a PNG and exit.
    #[command(hide = true)]
    Snapshot {
        out: PathBuf,
        #[arg(long, default_value = "overlay")]
        layout: Layout,
        #[arg(long)]
        view: Option<String>,
        #[arg(long)]
        select: Option<u32>,
        #[arg(long)]
        peek: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    let filter =
        tracing_subscriber::EnvFilter::try_from_env("HYPRGALLERY_LOG").unwrap_or_else(|_| {
            tracing_subscriber::EnvFilter::new(
                "hyprgallery=info,hyprgallery_core=info,hyprgallery_ui=info",
            )
        });
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();

    if let Err(e) = run(cli.command.unwrap_or(Command::Toggle { layout: None })) {
        eprintln!("hyprgallery: {e:#}");
        std::process::exit(1);
    }
}

fn run(command: Command) -> Result<()> {
    let quick = Duration::from_secs(5);
    match command {
        Command::Daemon { no_restore } => {
            daemon::run(daemon::Mode::Daemon { restore: !no_restore })
        }
        Command::Snapshot { out, layout, view, select, peek } => {
            daemon::run(daemon::Mode::Snapshot(daemon::Snapshot {
                out,
                layout,
                view,
                select,
                peek,
            }))
        }
        Command::Bar => bar::run(),
        Command::Toggle { layout } => {
            client::send(Request::Toggle { layout }, true, quick).map(drop)
        }
        Command::Show { layout } => client::send(Request::Show { layout }, true, quick).map(drop),
        Command::Hide => client::send(Request::Hide, false, quick).map(drop),
        Command::Set { path } => {
            let path = path.canonicalize().with_context(|| format!("{}", path.display()))?;
            // HyDE recolours the whole theme on every change, which takes a
            // few seconds. Wait for the outcome rather than guessing.
            client::send(Request::Set { path }, true, Duration::from_secs(60)).map(drop)
        }
        Command::Next { album } => client::send(Request::Next { album }, true, quick).map(drop),
        Command::Prev { album } => client::send(Request::Prev { album }, true, quick).map(drop),
        Command::Random { album } => client::send(Request::Random { album }, true, quick).map(drop),
        Command::Rotate { every, album, shuffle } => {
            let request = if every.eq_ignore_ascii_case("off") {
                Request::Rotate(RotateRequest::Off)
            } else {
                let every_secs = hyprgallery_core::rotation::parse_interval(&every)
                    .map_err(anyhow::Error::msg)?;
                Request::Rotate(RotateRequest::On { every_secs, album, shuffle })
            };
            let reply = client::send(request, true, quick)?;
            if let Some(status) = reply.status {
                println!("{}", bar::describe(&status));
            }
            Ok(())
        }
        Command::Restore => client::send(Request::Restore, true, quick).map(drop),
        Command::Reindex => client::send(Request::Reindex, true, quick).map(drop),
        Command::Status { json } => {
            let reply = match client::send(Request::Status, false, quick) {
                Ok(reply) => reply,
                Err(e) if client::not_running(&e) => {
                    if json {
                        println!("{{\"running\":false}}");
                    } else {
                        println!("not running");
                    }
                    return Ok(());
                }
                Err(e) => return Err(e),
            };
            let Some(status) = reply.status else { bail!("the daemon sent no status") };
            if json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                println!("{}", bar::describe(&status));
            }
            Ok(())
        }
        Command::Quit => match client::send(Request::Quit, false, quick) {
            Err(e) if client::not_running(&e) => Ok(()),
            other => other.map(drop),
        },
    }
}
