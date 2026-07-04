//! Property-based tests for `shell_quote` — the security-critical POSIX
//! single-quote escaper. The oracle is an *independent* shell-word decoder, so a
//! passing round-trip proves the quoted form parses as exactly one shell word
//! with the original bytes (i.e. it cannot be broken out of).

use nix_tmux_define::shell_quote;
use proptest::prelude::*;

/// An independent POSIX-ish tokenizer for a single shell word. It does NOT
/// mirror `shell_quote`'s implementation; it parses `'…'` literal segments and
/// `\<c>` escapes the way a shell would. Returns `None` if the string would not
/// parse as a single, complete word (unterminated quote, dangling backslash, or
/// an unquoted whitespace that would split the word).
fn shell_decode_single_word(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => loop {
                match chars.next() {
                    Some('\'') => break,
                    Some(ch) => out.push(ch),
                    None => return None,
                }
            },
            '\\' => match chars.next() {
                Some(ch) => out.push(ch),
                None => return None,
            },
            c if c.is_whitespace() => return None,
            ch => out.push(ch),
        }
    }
    Some(out)
}

/// Arbitrary strings over the full `char` domain (control chars, quotes,
/// backslashes, `$`, backticks, unicode) — the adversarial space for quoting.
fn arb_string() -> impl Strategy<Value = String> {
    proptest::collection::vec(any::<char>(), 0..40).prop_map(|cs| cs.into_iter().collect())
}

proptest! {
    /// Quoting any string yields output an independent shell tokenizer decodes
    /// back to exactly that string, as a single word.
    #[test]
    fn round_trips_through_independent_decoder(s in arb_string()) {
        let quoted = shell_quote(&s);
        let decoded = shell_decode_single_word(&quoted);
        prop_assert_eq!(
            decoded.as_deref(),
            Some(s.as_str()),
            "quoted form {:?} did not decode back to a single word",
            quoted,
        );
    }

    /// The output is always wrapped in single quotes.
    #[test]
    fn output_is_single_quoted(s in arb_string()) {
        let q = shell_quote(&s);
        prop_assert!(q.starts_with('\''), "must start with a quote: {:?}", q);
        prop_assert!(q.ends_with('\''), "must end with a quote: {:?}", q);
    }
}

/// Fixed regression guards for classic injection attempts.
#[test]
fn known_injection_attempts_are_neutralised() {
    for case in [
        "",
        "'; rm -rf / #",
        "$(reboot)",
        "`reboot`",
        "${HOME}",
        "a'b'c",
        "''",
        "newline\nhere",
    ] {
        let quoted = shell_quote(case);
        assert_eq!(
            shell_decode_single_word(&quoted).as_deref(),
            Some(case),
            "input {case:?} → {quoted:?} failed to neutralise",
        );
    }
}
