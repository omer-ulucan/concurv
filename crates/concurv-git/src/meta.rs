//! Mapping snapshot metadata to Git commit metadata and back (ADR 0002).
//!
//! A Git commit is derived only from data that survives the round trip
//! (tree, parents, author, whole seconds, message, ChangeId), so export is
//! deterministic and concurv -> Git -> concurv -> Git reproduces identical
//! Git commits. Round-trip rules (documented in ADR 0002):
//! - author `Name <email>` <-> Git author; Git's committer is set equal to
//!   the author on export and ignored on import;
//! - time: milliseconds are truncated to seconds, timezone `+0000`;
//! - message: trailing newlines are normalized away; the ChangeId travels as
//!   a final `Concurv-Change-Id:` trailer and is stripped again on import.

use concurv_core::{ChangeId, RandomId};

/// Trailer carrying the logical change id.
pub const TRAILER_CHANGE: &str = "Concurv-Change-Id: ";
fn clean_ident(s: &str) -> String {
    s.chars()
        .map(|c| {
            if matches!(c, '<' | '>' | '\n' | '\r' | '\0') {
                '_'
            } else {
                c
            }
        })
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Split a concurv author string into Git (name, email). `Name <email>` is
/// split; anything else becomes the name with an empty email. Characters Git
/// forbids in identities (`<`, `>`, newlines) are replaced by `_`.
pub fn git_ident(author: &str) -> (String, String) {
    if let Some(rest) = author.strip_suffix('>')
        && let Some((name, email)) = rest.rsplit_once(" <")
    {
        return (clean_ident(name), clean_ident(email));
    }
    (clean_ident(author), String::new())
}

/// Inverse of [`git_ident`].
pub fn concurv_author(name: &str, email: &str) -> String {
    if email.is_empty() {
        name.to_owned()
    } else {
        format!("{name} <{email}>")
    }
}

/// Git commit message for a snapshot: the message without trailing newlines,
/// a blank line, and the ChangeId trailer.
pub fn git_message(message: &str, change: &ChangeId) -> String {
    format!(
        "{}\n\n{TRAILER_CHANGE}{}\n",
        message.trim_end_matches('\n'),
        change.to_hex()
    )
}

/// Parse a Git commit message: (message, ChangeId if the message ends with a
/// concurv trailer). Bytes other than the trailer, including `\r`, are kept.
pub fn parse_message(git: &str) -> (String, Option<ChangeId>) {
    let body = git.trim_end_matches('\n');
    let sep = format!("\n\n{TRAILER_CHANGE}");
    if let Some(at) = body.rfind(&sep)
        && let Ok(c) = ChangeId::from_hex(&body[at + sep.len()..])
    {
        return (body[..at].to_owned(), Some(c));
    }
    (body.to_owned(), None)
}
/// A deterministic ChangeId for a Git commit without a concurv trailer.
pub fn change_for_git_commit(oid: &str) -> Option<ChangeId> {
    let bytes: Vec<u8> = (0..32)
        .step_by(2)
        .map(|i| {
            oid.get(i..i + 2)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        })
        .collect::<Option<_>>()?;
    let arr: [u8; 16] = bytes.try_into().ok()?;
    Some(ChangeId::from_random(RandomId::from_bytes(arr)))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn ident_and_message_roundtrip() {
        assert_eq!(
            git_ident("Ada <ada@x.org>"),
            ("Ada".into(), "ada@x.org".into())
        );
        assert_eq!(
            git_ident("just a name"),
            ("just a name".into(), String::new())
        );
        for a in ["Ada <ada@x.org>", "just a name"] {
            let (n, e) = git_ident(a);
            assert_eq!(concurv_author(&n, &e), a);
        }
        let c = ChangeId::from_random(RandomId::from_bytes([7; 16]));

        for m in ["subject", "subject\n\nbody line", "multi\nline\n"] {
            let g = git_message(m, &c);
            assert_eq!(
                parse_message(&g),
                (m.trim_end_matches('\n').to_owned(), Some(c))
            );
        }
        assert_eq!(
            parse_message("plain git message\n"),
            ("plain git message".into(), None)
        );
        assert!(change_for_git_commit(&"ab".repeat(20)).is_some());
        assert!(change_for_git_commit("short").is_none());
    }
}
