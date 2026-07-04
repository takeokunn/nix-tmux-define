use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use nix_tmux_define::{
    load_session, load_sessions_from_dir, load_sessions_from_dir_lenient, Compiler, Executor,
    RealTmux, Session, TmuxName,
};
use std::collections::HashSet;
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
    /// Start or attach to a tmux session from a config file
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

    /// Atomically replace a named session from a config file
    Reload {
        /// Path to the session config (JSON, TOML, or YAML)
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
    },

    /// List sessions from config files without probing tmux by default
    List {
        #[arg(long, value_name = "PATH")]
        config: Vec<PathBuf>,
        #[arg(long, value_name = "DIR")]
        config_dir: Option<PathBuf>,
        /// Probe tmux and mark configs whose session is currently running
        #[arg(long)]
        running_status: bool,
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

fn running_sessions_outcome(
    success: bool,
    status: &str,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<HashSet<TmuxName>> {
    if success {
        let stdout = std::str::from_utf8(stdout)
            .context("`tmux list-sessions` returned non-UTF-8 session names")?;

        return stdout
            .lines()
            .map(|name| {
                TmuxName::new(name.to_owned()).with_context(|| {
                    format!("invalid tmux session name from `tmux list-sessions`: {name:?}")
                })
            })
            .collect();
    }

    let stderr = tmux_stderr(stderr);
    if is_tmux_no_server(&stderr) {
        return Ok(HashSet::new());
    }

    if stderr.is_empty() {
        anyhow::bail!("`tmux list-sessions` failed with {status}");
    }
    anyhow::bail!("`tmux list-sessions` failed: {stderr}");
}

fn running_sessions() -> Result<HashSet<TmuxName>> {
    let output = std::process::Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
        .context("failed to execute `tmux list-sessions`")?;
    running_sessions_outcome(
        output.status.success(),
        &output.status.to_string(),
        &output.stdout,
        &output.stderr,
    )
}

fn tmux_stderr(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr).trim().to_owned()
}

fn is_tmux_no_server(stderr: &str) -> bool {
    stderr.contains("no server running")
}

fn format_session_line(s: &Session, running: Option<&HashSet<TmuxName>>) -> String {
    let is_running = running
        .map(|sessions| sessions.contains(s.name.as_str()))
        .unwrap_or(false);
    let tag = if is_running { " [running]" } else { "" };
    format!("{}{} — {} window(s)", s.name, tag, s.windows.len())
}

fn print_session_list(sessions: &[Session], running: Option<&HashSet<TmuxName>>) {
    for s in sessions {
        println!("{}", format_session_line(s, running));
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
            running_status,
        } => {
            let mut sessions = Vec::new();
            for p in &configs {
                sessions.push(load_session(p)?);
            }
            if let Some(dir) = &config_dir {
                // An explicitly requested directory is strict: a malformed
                // config there is an error the caller wants surfaced.
                sessions.extend(load_sessions_from_dir(dir)?);
            }
            if configs.is_empty() && config_dir.is_none() {
                // The implicit current-directory scan is best-effort: unrelated
                // config-extension files (Cargo.toml, package.json, …) are
                // expected here and must not abort the command. Skips are
                // reported as warnings so nothing fails silently.
                let scan = load_sessions_from_dir_lenient(Path::new("."))?;
                for skipped in &scan.skipped {
                    eprintln!(
                        "warning: skipping '{}' ({})",
                        skipped.path.display(),
                        skipped.reason
                    );
                }
                sessions.extend(scan.sessions);
            }
            let running = if running_status {
                Some(running_sessions()?)
            } else {
                None
            };
            print_session_list(&sessions, running.as_ref());
        }

        Command::Schema => {
            println!("{}", nix_tmux_define::json_schema()?);
        }

        Command::Completions { shell } => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "nix-tmux-define", &mut std::io::stdout());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix_tmux_define::{LayoutNode, Session, Window};
    use std::collections::BTreeMap;

    fn tmux_name(value: &str) -> TmuxName {
        TmuxName::new(value).unwrap()
    }

    fn make_session(name: &str, window_count: usize) -> Session {
        let windows: Vec<Window> = (0..window_count)
            .map(|i| Window {
                name: tmux_name(&format!("w{}", i)),
                root: None,
                env: vec![],
                options: BTreeMap::new(),
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
            name: tmux_name(name),
            root: None,
            windows,
            env: vec![],
            pre_hook: None,
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
        }
    }

    #[test]
    fn format_session_line_not_running() {
        let s = make_session("dev", 2);
        let line = format_session_line(&s, None);
        assert_eq!(line, "dev — 2 window(s)");
    }

    #[test]
    fn format_session_line_running() {
        let s = make_session("prod", 3);
        let running = [tmux_name("prod")].into_iter().collect();
        let line = format_session_line(&s, Some(&running));
        assert_eq!(line, "prod [running] — 3 window(s)");
    }

    #[test]
    fn format_session_line_ignores_running_without_status_probe() {
        let s = make_session("prod", 3);
        let running = [tmux_name("prod")].into_iter().collect::<HashSet<_>>();
        let line = format_session_line(&s, None);
        assert_eq!(line, "prod — 3 window(s)");
        assert!(running.contains("prod"));
    }

    #[test]
    fn generate_returns_bash_script() {
        let s = make_session("test", 1);
        let script = generate(&s).unwrap();
        assert!(script.starts_with("#!/usr/bin/env bash"));
        assert!(script.contains("tmux new-session"));
    }

    #[test]
    fn running_sessions_parses_valid_tmux_names() {
        let sessions =
            running_sessions_outcome(true, "exit status: 0", b"dev\nprod\n", b"").unwrap();
        assert!(sessions.contains("dev"));
        assert!(sessions.contains("prod"));
    }

    #[test]
    fn running_sessions_rejects_invalid_tmux_name_from_tmux() {
        let err = running_sessions_outcome(true, "exit status: 0", b"bad:name\n", b"").unwrap_err();
        assert!(err
            .to_string()
            .contains("invalid tmux session name from `tmux list-sessions`"));
    }

    #[test]
    fn running_sessions_rejects_non_utf8_names() {
        let err = running_sessions_outcome(true, "exit status: 0", b"dev\xff\n", b"").unwrap_err();
        assert_eq!(
            err.to_string(),
            "`tmux list-sessions` returned non-UTF-8 session names"
        );
    }

    #[test]
    fn running_sessions_ignores_missing_tmux_server() {
        let sessions = running_sessions_outcome(
            false,
            "exit status: 1",
            b"",
            b"no server running on /tmp/tmux-501/default",
        )
        .unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn running_sessions_reports_unexpected_failure() {
        let err = running_sessions_outcome(false, "exit status: 1", b"", b"permission denied")
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "`tmux list-sessions` failed: permission denied"
        );
    }
}
