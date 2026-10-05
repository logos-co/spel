//! Decides which dependency source files are unsafe to hand to
//! `syn::parse_file`.
//!
//! Inside a proc macro, parsing routes through the compiler's lexer.
//! Re-lexing a dependency's raw text under the consumer's edition can
//! queue a fatal "unknown prefix" diagnostic as a side effect, aborting
//! the consumer's build with an error that exists nowhere in the
//! consumer's code, even though the parse error itself is caught.
//! ark-ff 0.3.0 ships the trigger shape.
//!
//! The scan is byte-level but context-aware: a match inside a string
//! literal or comment is ignored, those bytes never reach the lexer as
//! tokens. The bias rule for everything the scan cannot classify is to
//! treat it as code. The worst case of a spurious match is a skipped
//! file, visible through the caller's warning. The worst case of a
//! missed match is the fatal diagnostic coming back.

// The scanner is a byte walk: every index is guarded by a bounds check
// in the same expression or the enclosing loop condition, and this
// code runs at build and CLI time, never in a guest.
#![allow(clippy::indexing_slicing)]

/// True when the text contains a macro metavariable glued to a string
/// literal in code position (e.g. ark-ff 0.3.0's `stringify!($Fp"({})")`).
/// That shape is legal in the defining crate's own pre-2021 edition, but
/// re-lexed under a 2021+ edition it queues the fatal diagnostic this
/// module exists to prevent. Files like this cannot carry usable
/// `#[account_type]` items, so callers skip them instead of parsing.
pub(crate) fn has_metavar_glued_literal(content: &str) -> bool {
    let b = content.as_bytes();
    let n = b.len();
    let mut i = 0;
    let mut prev_ident = false;
    while i < n {
        let was_ident = b[i].is_ascii_alphabetic() || b[i] == b'_';
        match b[i] {
            b'/' if i + 1 < n && b[i + 1] == b'/' => {
                while i < n && b[i] != b'\n' {
                    i += 1;
                }
            },
            b'/' if i + 1 < n && b[i + 1] == b'*' => i = skip_block_comment(b, i),
            // An identifier glued to a quote is a reserved prefix under
            // edition 2021.  Legal prefixes (r, b, br, c, cr) are handled
            // by their own arms below, which fire only when !prev_ident.
            // A quote reaching this arm with prev_ident set therefore has
            // a non-prefix identifier before it — the glued shape.
            b'"' if prev_ident && !is_legal_string_prefix(b, i) => return true,
            b'"' => i = skip_string(b, i),
            // No legal Rust prefix ends with ' before a char literal, so
            // any identifier glued to ' is a reserved prefix.
            b'\'' if prev_ident => return true,
            b'\'' => i = skip_char_or_lifetime(b, i),
            b'r' | b'b' if !prev_ident => match raw_or_byte_literal_end(b, i) {
                Some(j) => i = j,
                None => i += 1,
            },
            b'c' if !prev_ident => match c_string_literal_end(b, i) {
                Some(j) => i = j,
                None => i += 1,
            },
            b'$' => {
                let start = i + 1;
                let mut j = start;
                while j < n && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                    j += 1;
                }
                if j > start && j < n && b[j] == b'"' && !b[start].is_ascii_digit() {
                    return true;
                }
                i = start;
            },
            _ => i += 1,
        }
        prev_ident = was_ident;
    }
    false
}

/// The identifier ending immediately before position `quote_pos`, scanned
/// backwards. Used to check whether a quote preceded by an identifier is a
/// legal string prefix (r, b, br, c, cr) or a reserved glued shape.
fn preceding_ident(b: &[u8], quote_pos: usize) -> &[u8] {
    let mut start = quote_pos;
    while start > 0 && (b[start - 1].is_ascii_alphanumeric() || b[start - 1] == b'_') {
        start -= 1;
    }
    &b[start..quote_pos]
}

/// True when the identifier before `quote_pos` is a legal Rust string
/// prefix: `r`, `b`, `br`, `c`, or `cr`.
fn is_legal_string_prefix(b: &[u8], quote_pos: usize) -> bool {
    matches!(
        preceding_ident(b, quote_pos),
        b"r" | b"b" | b"br" | b"c" | b"cr"
    )
}

/// Advance past a C-string literal starting at `c`: either `c"..."` (plain,
/// with escapes) or `cr#"..."#` (raw, no escapes). `None` when the position
/// is not a C-string opener (e.g. a variable named `c`).
fn c_string_literal_end(b: &[u8], i: usize) -> Option<usize> {
    let n = b.len();
    if i + 1 < n && b[i + 1] == b'"' {
        return Some(skip_string(b, i + 1));
    }
    if i + 2 < n && b[i + 1] == b'r' {
        return raw_string_body(b, i + 2);
    }
    None
}

/// Advance past a block comment starting at `/*`. Rust block comments
/// nest. An unterminated comment runs to the end of the file.
fn skip_block_comment(b: &[u8], mut i: usize) -> usize {
    let n = b.len();
    let mut depth = 1;
    i += 2;
    while i < n && depth > 0 {
        if i + 1 < n && b[i] == b'/' && b[i + 1] == b'*' {
            depth += 1;
            i += 2;
        } else if i + 1 < n && b[i] == b'*' && b[i + 1] == b'/' {
            depth -= 1;
            i += 2;
        } else {
            i += 1;
        }
    }
    i
}

/// Advance past an ordinary string literal starting at its opening
/// quote. A backslash escapes the next byte. Unterminated runs to the
/// end of the file.
fn skip_string(b: &[u8], mut i: usize) -> usize {
    let n = b.len();
    i += 1;
    while i < n {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return i + 1,
            _ => i += 1,
        }
    }
    n
}

/// Advance past a raw string (`r"..."`, `r#"..."#`), byte string
/// (`b"..."`), raw byte string (`br#"..."#`) or byte char (`b'...'`)
/// starting at the `r` or `b`. `None` when the position is not a
/// literal opener, e.g. a raw identifier like `r#match`.
fn raw_or_byte_literal_end(b: &[u8], i: usize) -> Option<usize> {
    let n = b.len();
    match b[i] {
        b'r' => raw_string_body(b, i + 1),
        b'b' if i + 1 < n && b[i + 1] == b'r' => raw_string_body(b, i + 2),
        b'b' if i + 1 < n && b[i + 1] == b'"' => Some(skip_string(b, i + 1)),
        b'b' if i + 1 < n && b[i + 1] == b'\'' => Some(skip_char_or_lifetime(b, i + 1)),
        _ => None,
    }
}

/// The end of a raw-string body whose hashes start at `j`. Raw strings
/// have no escapes: the body closes at a quote followed by the same
/// number of hashes it opened with. `None` when there is no opening
/// quote after the hashes.
fn raw_string_body(b: &[u8], mut j: usize) -> Option<usize> {
    let n = b.len();
    let mut hashes = 0;
    while j < n && b[j] == b'#' {
        hashes += 1;
        j += 1;
    }
    if j >= n || b[j] != b'"' {
        return None;
    }
    j += 1;
    while j < n {
        if b[j] == b'"'
            && j + 1 + hashes <= n
            && b[j + 1..j + 1 + hashes].iter().all(|&c| c == b'#')
        {
            return Some(j + 1 + hashes);
        }
        j += 1;
    }
    Some(n)
}

/// Advance past a char literal, or just past the quote when it is a
/// lifetime.
///
/// The escape arm consumes quote, backslash and one escaped byte
/// unconditionally, then scans plainly for the closing quote. No char
/// escape contains a quote after its first escaped byte, and symmetric
/// escape handling would overshoot the terminator of `'\\'`.
///
/// For a non-escaped char, scan a small window forward for the closing
/// quote. A Rust char is at most 4 UTF-8 bytes, so 5 bytes after the
/// opening quote (content + closing quote) is enough. A lifetime (`'a`)
/// has no closing quote in that window, so it falls through to `i + 1`.
/// The previous fixed-offset check (`b[i + 2] == b'\''`) missed multi-byte
/// chars like `'é'`, landing the scanner inside the next token.
fn skip_char_or_lifetime(b: &[u8], i: usize) -> usize {
    let n = b.len();
    if i + 1 < n && b[i + 1] == b'\\' {
        let mut j = i + 3;
        while j < n && b[j] != b'\'' {
            j += 1;
        }
        return (j + 1).min(n);
    }
    let max_scan = (i + 6).min(n);
    let scan_start = i + 1;
    if let Some(offset) = b[scan_start..max_scan]
        .iter()
        .position(|&byte| byte == b'\'')
    {
        return scan_start + offset + 1;
    }
    i + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metavar_glued_literal_is_detected() {
        // The exact shape from ark-ff-0.3.0 src/fields/macros.rs:615.
        assert!(has_metavar_glued_literal(
            r#"write!(f, stringify!($Fp"({})"), self.into_repr())"#
        ));
        // A metavariable not glued to a quote is fine.
        assert!(!has_metavar_glued_literal(
            "macro_rules! m { ($x:ident) => { $x } }"
        ));
        // A bare dollar, a digit after the dollar, and plain code are fine.
        assert!(!has_metavar_glued_literal("let price = \"$\";"));
        assert!(!has_metavar_glued_literal("fn account_type_free() {}"));
    }

    // A dollar-word glued to a quote inside a string literal or comment
    // is inert: those bytes never reach the lexer as tokens, so the
    // file must not be skipped over them.
    #[test]
    fn matches_inside_strings_and_comments_are_inert() {
        // Line and doc comments.
        assert!(!has_metavar_glued_literal(
            "// pays in \"$HOME\"\nfn f() {}"
        ));
        assert!(!has_metavar_glued_literal(
            "/// Respects \"$PATH\" when resolving.\npub fn g() {}"
        ));
        // Nested block comment.
        assert!(!has_metavar_glued_literal(
            r#"/* outer /* $X" */ inner */ fn f() {}"#
        ));
        // A string ending in a dollar-word.
        assert!(!has_metavar_glued_literal(r#"let s = "price $USD";"#));
        // Raw and byte strings.
        assert!(!has_metavar_glued_literal(r##"let s = r#"$Fp"x"#;"##));
        assert!(!has_metavar_glued_literal(r#"let v = b"price $USD";"#));
    }

    // String-lookalike syntax before a real landmine must not swallow
    // it: each of these once misclassified means the fatal diagnostic
    // comes back.
    #[test]
    fn code_position_matches_survive_string_lookalikes() {
        // A quote inside a char literal is not a string opener.
        assert!(has_metavar_glued_literal(
            r#"let q = '"'; stringify!($Fp"({})")"#
        ));
        // '\\' terminates at its own closing quote.
        assert!(has_metavar_glued_literal(
            r#"let c = '\\'; stringify!($Fp"x")"#
        ));
        // A raw identifier is not a raw string.
        assert!(has_metavar_glued_literal(
            r##"fn r#match() {} stringify!($Fp"x")"##
        ));
        // A lifetime is not a char literal.
        assert!(has_metavar_glued_literal(
            r#"fn f<'a>(x: &'a u8) {} stringify!($Fp"x")"#
        ));
    }

    // --- Regression tests for issue #258 ---

    // Section 1: missed glued shapes (the fatal diagnostic still comes back).

    #[test]
    fn metavar_glued_to_char_literal_is_detected() {
        // $f'a' — metavar glued to a char literal start.
        assert!(has_metavar_glued_literal(
            r#"macro_rules! m { ($f:ident) => { stringify!($f'a') } }"#
        ));
    }

    #[test]
    fn metavar_glued_to_string_in_pattern_is_detected() {
        // $f"px" — metavar glued to a string in a pattern position.
        assert!(has_metavar_glued_literal(
            r#"macro_rules! m { ($f:ident"px") => { () } }"#
        ));
    }

    #[test]
    fn plain_ident_glued_to_string_is_detected() {
        // foo"bar" — a plain identifier glued to a string (no metavar).
        assert!(has_metavar_glued_literal(
            r#"macro_rules! m { () => { stringify!(foo"bar") } }"#
        ));
    }

    // Section 2: multi-byte char literal desyncs the scanner.

    #[test]
    fn multibyte_char_does_not_desync_scanner() {
        // 'é' is 2 UTF-8 bytes; the old fixed-offset check landed inside
        // the next token and flipped parity, masking the target shape.
        assert!(has_metavar_glued_literal(
            "let v = ['\u{00e9}','\"']; stringify!($Fp\"({})\")"
        ));
    }

    // Section 3: raw C-strings must not cause false positives.

    #[test]
    fn raw_c_string_with_backslash_body_is_not_flagged() {
        // cr#"C:\temp\"# — the old scanner did not recognize cr-prefixed
        // raw C-strings, so the backslash in the body desynced quote
        // parity and the clean file was falsely skipped.
        assert!(!has_metavar_glued_literal(
            "let p = cr#\"C:\\temp\\\"#; let s = \"cost $USD\"; #[account_type] pub struct A { x: u8 }"
        ));
    }

    #[test]
    fn plain_c_string_is_not_flagged() {
        // c"..." — plain C-string literal in clean code.
        assert!(!has_metavar_glued_literal(
            r#"let s = c"hello"; #[account_type] pub struct A { x: u8 }"#
        ));
    }

    // Legal prefixes must still be treated as literals, not glued shapes.

    #[test]
    fn legal_prefixes_are_not_flagged() {
        assert!(!has_metavar_glued_literal(r#"let s = r"raw"; fn f() {}"#));
        assert!(!has_metavar_glued_literal(
            r##"let s = r#"raw"#; fn f() {}"##
        ));
        assert!(!has_metavar_glued_literal(r#"let s = b"bytes"; fn f() {}"#));
        assert!(!has_metavar_glued_literal(
            r##"let s = br#"raw bytes"#; fn f() {}"##
        ));
        assert!(!has_metavar_glued_literal(r#"let s = c"cstr"; fn f() {}"#));
        assert!(!has_metavar_glued_literal(
            r##"let s = cr#"raw cstr"#; fn f() {}"##
        ));
    }
}
