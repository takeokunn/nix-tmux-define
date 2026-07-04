//! Attach policy — the pure decision of whether `run`/`reload` should attach to
//! (or switch the client to) the session after building it.
//!
//! Building the session is always useful on its own; *attaching* only makes
//! sense when there is a terminal to hand over. A systemd oneshot, a CI step, or
//! any other non-interactive caller has no controlling terminal, so a blind
//! `tmux attach-session` there fails with "open terminal failed: not a
//! terminal" and takes the whole command down with it — even though the session
//! was created correctly.
//!
//! This module isolates that decision as a pure function so it can be exhaustively
//! unit-tested without a real terminal, tmux server, or process environment. The
//! side-effecting backend ([`crate::backend::RealTmux`]) merely feeds it the two
//! environmental facts (`in_tmux`, `stdin_is_terminal`) and acts on the verdict.

/// How aggressively `run`/`reload` should attach after building the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AttachMode {
    /// Attach (or switch, when already inside tmux) whenever a terminal is
    /// available; otherwise build the session and return without attaching.
    /// This is the safe default for both interactive and headless callers.
    #[default]
    Auto,
    /// Never attach or switch — build the session and return. Lets an
    /// interactive caller preseed sessions in the background without being
    /// yanked into one (`--no-attach`).
    Never,
}

/// The concrete post-build action the backend should take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachAction {
    /// Inside an existing tmux client: move that client to the session.
    Switch,
    /// A terminal is available and we are not inside tmux: take it over.
    Attach,
    /// No terminal (or `Never`): leave the session detached and return Ok.
    Skip,
}

/// Resolve the post-build action from the mode and the two environmental facts.
///
/// - `in_tmux` — whether `$TMUX` is set (we are inside a tmux client). A
///   `switch-client` works through the tmux server and does not need the current
///   stdin to be a terminal, so it is allowed even when `stdin_is_terminal` is
///   false.
/// - `stdin_is_terminal` — whether stdin is a real terminal, i.e. something
///   `tmux attach-session` can take over.
pub fn resolve_attach_action(
    mode: AttachMode,
    in_tmux: bool,
    stdin_is_terminal: bool,
) -> AttachAction {
    match mode {
        AttachMode::Never => AttachAction::Skip,
        AttachMode::Auto => {
            if in_tmux {
                AttachAction::Switch
            } else if stdin_is_terminal {
                AttachAction::Attach
            } else {
                AttachAction::Skip
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_inside_tmux_switches_even_without_terminal() {
        // switch-client goes through the server, so a missing stdin terminal
        // (e.g. a reload triggered from a non-interactive hook inside tmux)
        // must not downgrade it to Skip.
        assert_eq!(
            resolve_attach_action(AttachMode::Auto, true, false),
            AttachAction::Switch
        );
        assert_eq!(
            resolve_attach_action(AttachMode::Auto, true, true),
            AttachAction::Switch
        );
    }

    #[test]
    fn auto_with_terminal_outside_tmux_attaches() {
        assert_eq!(
            resolve_attach_action(AttachMode::Auto, false, true),
            AttachAction::Attach
        );
    }

    #[test]
    fn auto_without_terminal_outside_tmux_skips() {
        // The regression this whole module exists for: a systemd oneshot /
        // headless caller must build the session and return, not fail on attach.
        assert_eq!(
            resolve_attach_action(AttachMode::Auto, false, false),
            AttachAction::Skip
        );
    }

    #[test]
    fn never_always_skips_regardless_of_environment() {
        for &in_tmux in &[false, true] {
            for &is_tty in &[false, true] {
                assert_eq!(
                    resolve_attach_action(AttachMode::Never, in_tmux, is_tty),
                    AttachAction::Skip,
                    "Never must skip for in_tmux={in_tmux}, is_tty={is_tty}"
                );
            }
        }
    }

    #[test]
    fn default_mode_is_auto() {
        assert_eq!(AttachMode::default(), AttachMode::Auto);
    }
}
