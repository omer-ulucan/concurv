//! Git's C-style path quoting, as used by `fast-import` / `fast-export`.

use concurv_core::{ConcurvError, ErrorCode, Result};

/// Quote a path for a `fast-import` `M` line when Git requires it (leading
/// `"`, or any `"`, `\` or control character). Other bytes, including spaces
/// and non-ASCII UTF-8, are written as-is.
pub fn quote(path: &str) -> String {
    let needs = path.starts_with('"')
        || path
            .bytes()
            .any(|b| b == b'"' || b == b'\\' || b < 0x20 || b == 0x7f);
    if !needs {
        return path.to_owned();
    }
    // Escape per character so multi-byte UTF-8 sequences stay intact.
    let mut out = String::with_capacity(path.len() + 2);
    out.push('"');
    for ch in path.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\{:03o}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn bad(what: &str) -> ConcurvError {
    ConcurvError::new(
        ErrorCode::ExternalTool,
        format!("malformed quoted path from git: {what}"),
    )
}

/// Decode a path from a `fast-export` line: either raw bytes, or a C-style
/// quoted string with `\a \b \f \n \r \t \v \\ \"` and `\ooo` escapes.
pub fn unquote(raw: &[u8]) -> Result<Vec<u8>> {
    if raw.first() != Some(&b'"') {
        return Ok(raw.to_vec());
    }
    if raw.len() < 2 || raw.last() != Some(&b'"') {
        return Err(bad("missing closing quote"));
    }
    let inner = &raw[1..raw.len() - 1];
    let mut out = Vec::with_capacity(inner.len());
    let mut i = 0;
    while i < inner.len() {
        let b = inner[i];
        if b != b'\\' {
            out.push(b);
            i += 1;
            continue;
        }
        let e = *inner.get(i + 1).ok_or_else(|| bad("dangling backslash"))?;
        i += 2;
        out.push(match e {
            b'a' => 7,
            b'b' => 8,
            b'f' => 12,
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'v' => 11,
            b'\\' => b'\\',
            b'"' => b'"',
            b'0'..=b'3' => {
                let d = inner
                    .get(i..i + 2)
                    .ok_or_else(|| bad("short octal escape"))?;
                if !d.iter().all(|c| (b'0'..=b'7').contains(c)) {
                    return Err(bad("invalid octal escape"));
                }
                i += 2;
                (e - b'0') * 64 + (d[0] - b'0') * 8 + (d[1] - b'0')
            }
            _ => return Err(bad("unknown escape")),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn roundtrip() {
        for p in [
            "plain.txt",
            "sp ace",
            "dir/naïve",
            "q\"uote",
            "back\\slash",
            "\"lead",
            "tab\there",
            "nl\nx",
        ] {
            let q = quote(p);
            assert_eq!(
                unquote(q.as_bytes()).unwrap(),
                p.as_bytes(),
                "{p:?} -> {q:?}"
            );
        }
        assert_eq!(quote("sp ace"), "sp ace");
        assert_eq!(
            unquote(br#""quo\357\200\242te""#).unwrap(),
            "quo\u{f022}te".as_bytes()
        );
        assert!(unquote(br#""bad\q""#).is_err());
        assert!(unquote(br#""open"#).is_err());
    }
}
