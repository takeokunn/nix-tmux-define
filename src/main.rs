use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use nix_tmux_define::{
    load_session, load_sessions_from_dir, Compiler, Executor, RealTmux, Session,
};
use std::path::{Path, PathBuf};
use std::process::Stdio;

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
        /// Kill the tmux server before creating the session (wipes all sessions)
        #[arg(long, short = 'k')]
        kill_server: bool,
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

fn generate(session: &Session) -> Result<String> {
    let mut compiler = Compiler::new();
    compiler.compile(session)?;
    Ok(compiler.into_script())
}

fn session_running(name: &str) -> bool {
    std::process::Command::new("tmux")
        .args(["has-session", "-t", name])
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn format_session_line(s: &Session, is_running: bool) -> String {
    let running = if is_running { " [running]" } else { "" };
    format!("{}{} — {} window(s)", s.name, running, s.windows.len())
}

fn print_session_list(sessions: &[Session]) {
    for s in sessions {
        println!("{}", format_session_line(s, session_running(&s.name)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix_tmux_define::{LayoutNode, Session, Window};
    use std::collections::HashMap;

    fn make_session(name: &str, window_count: usize) -> Session {
        let windows: Vec<Window> = (0..window_count)
            .map(|i| Window {
                name: format!("w{}", i),
                root: None,
                env: vec![],
                options: HashMap::new(),
                select_layout: None,
                layout: LayoutNode::Pane {
                    command: None,
                    focus: false,
                    title: None,
                    wait_for: None,
                },
            })
            .collect();
        Session {
            name: name.into(),
            root: None,
            windows,
            env: vec![],
            pre_hook: None,
            options: HashMap::new(),
            vars: HashMap::new(),
        }
    }

    #[test]
    fn format_session_line_not_running() {
        let s = make_session("dev", 2);
        let line = format_session_line(&s, false);
        assert_eq!(line, "dev — 2 window(s)");
    }

    #[test]
    fn format_session_line_running() {
        let s = make_session("prod", 3);
        let line = format_session_line(&s, true);
        assert_eq!(line, "prod [running] — 3 window(s)");
    }

    #[test]
    fn generate_returns_bash_script() {
        let s = make_session("test", 1);
        let script = generate(&s).unwrap();
        assert!(script.starts_with("#!/usr/bin/env bash"));
        assert!(script.contains("tmux new-session"));
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Run { config, kill_server } => {
            if kill_server {
                std::process::Command::new("tmux")
                    .args(["kill-server"])
                    .stderr(Stdio::null())
                    .status()
                    .ok();
            }
            let session = load_session(&config)?;
            let backend = RealTmux;
            let executor = Executor::new(&backend);
            executor.run(&session)?;
        }

        Command::Print { config } => {
            let session = load_session(&config)?;
            print!("{}", generate(&session)?);
        }

        Command::Validate { config } => {
            let session = load_session(&config)?;
            eprintln!(
                "✓  '{}' — {} window(s)",
                session.name,
                session.windows.len()
            );
        }

        Command::Reload { config } => {
            let session = load_session(&config)?;
            let backend = RealTmux;
            let executor = Executor::new(&backend);
            executor.reload(&session)?;
        }

        Command::List {
            config: configs,
            config_dir,
        } => {
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
