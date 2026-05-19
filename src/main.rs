use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use nix_tmux_define::{Compiler, Session};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "nix-tmux-define",
    about = "Declarative tmux session manager — JSON config → bash script",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate a session script and execute it immediately
    Run {
        /// Path to the JSON session config
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
    },

    /// Print the generated bash script to stdout without executing
    Print {
        /// Path to the JSON session config
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
    },

    /// Parse and validate a config file, reporting any errors
    Validate {
        /// Path to the JSON session config
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
    },

    /// Emit shell completion scripts for the given shell
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

fn load(path: &PathBuf) -> Result<Session> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read '{}'", path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("invalid JSON in '{}'", path.display()))
}

fn generate(session: &Session) -> String {
    let mut compiler = Compiler::new();
    compiler.compile(session);
    compiler.into_script()
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Run { config } => {
            let script = generate(&load(&config)?);
            let status = std::process::Command::new("bash")
                .arg("-c")
                .arg(&script)
                .status()
                .context("failed to spawn bash")?;
            if !status.success() {
                anyhow::bail!("session script exited with: {}", status);
            }
        }

        Command::Print { config } => {
            print!("{}", generate(&load(&config)?));
        }

        Command::Validate { config } => {
            let session = load(&config)?;
            eprintln!("✓  '{}' — {} window(s)", session.name, session.windows.len());
        }

        Command::Completions { shell } => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "nix-tmux-define", &mut std::io::stdout());
        }
    }
    Ok(())
}
