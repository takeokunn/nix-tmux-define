use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use nix_tmux_define::{Compiler, Executor, RealTmux, Session, load_session, load_sessions_from_dir};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "nix-tmux-define",
    about = "Declarative tmux session manager — JSON / TOML / YAML config → tmux session",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start a tmux session from a config file (uses RealTmux, no bash script)
    Run {
        /// Path to the session config (JSON, TOML, or YAML)
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
    },

    /// Print the generated bash script to stdout without executing
    Print {
        /// Path to the session config (JSON, TOML, or YAML)
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
    },

    /// Parse and validate a config file, reporting any errors
    Validate {
        /// Path to the session config (JSON, TOML, or YAML)
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
    },

    /// Kill and re-create a session from a config file
    Reload {
        /// Path to the session config (JSON, TOML, or YAML)
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
    },

    /// List all sessions from config files
    List {
        #[arg(long, value_name = "PATH")]
        config: Vec<PathBuf>,
        #[arg(long, value_name = "DIR")]
        config_dir: Option<PathBuf>,
    },

    /// Print the JSON Schema for the session config format
    Schema,

    /// Emit shell completion scripts for the given shell
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

fn generate(session: &Session) -> String {
    let mut compiler = Compiler::new();
    compiler.compile(session);
    compiler.into_script()
}

fn session_running(name: &str) -> bool {
    std::process::Command::new("tmux")
        .args(["has-session", "-t", name])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn print_session_list(sessions: &[Session]) {
    for s in sessions {
        let running = if session_running(&s.name) { " [running]" } else { "" };
        println!(
            "{}{} — {} window(s)",
            s.name,
            running,
            s.windows.len()
        );
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Run { config } => {
            let session = load_session(&config)?;
            let backend = RealTmux;
            let executor = Executor::new(&backend);
            executor.run(&session)?;
        }

        Command::Print { config } => {
            let session = load_session(&config)?;
            print!("{}", generate(&session));
        }

        Command::Validate { config } => {
            let session = load_session(&config)?;
            eprintln!("✓  '{}' — {} window(s)", session.name, session.windows.len());
        }

        Command::Reload { config } => {
            let session = load_session(&config)?;
            let backend = RealTmux;
            let executor = Executor::new(&backend);
            executor.reload(&session)?;
        }

        Command::List { config: configs, config_dir } => {
            let mut sessions = Vec::new();
            for p in &configs {
                sessions.push(load_session(p)?);
            }
            if let Some(dir) = &config_dir {
                sessions.extend(load_sessions_from_dir(dir)?);
            }
            if configs.is_empty() && config_dir.is_none() {
                sessions.extend(load_sessions_from_dir(Path::new("."))?);
            }
            print_session_list(&sessions);
        }

        Command::Schema => {
            println!("{}", nix_tmux_define::json_schema());
        }

        Command::Completions { shell } => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "nix-tmux-define", &mut std::io::stdout());
        }
    }
    Ok(())
}
