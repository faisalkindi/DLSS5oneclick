//! Tidying for the long messages this tool shows.
//!
//! Rust string literals in this codebase are wrapped across source lines, and
//! `cargo fmt` turns some of those wraps into runs of spaces inside the string.
//! Nobody notices in the source; everybody notices in a message box that reads
//! "which NGX also          answers". Squashing runs of whitespace at display
//! time fixes every message at once, including ones written later.

/// Collapse every run of whitespace to a single space and trim the ends.
/// Applied to text on its way to the user, never to text on its way to a file.
pub fn tidy(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            in_space = true;
            continue;
        }
        if in_space && !out.is_empty() {
            out.push(' ');
        }
        in_space = false;
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::tidy;

    #[test]
    fn runs_of_spaces_become_one() {
        assert_eq!(
            tidy("0xBAD00001 is FeatureNotSupported, which NGX also          answers"),
            "0xBAD00001 is FeatureNotSupported, which NGX also answers"
        );
        // Wrapped lines and stray indentation read as one paragraph.
        assert_eq!(tidy("first line\n            second"), "first line second");
        assert_eq!(tidy("   padded   "), "padded");
        // A single space is left exactly as it is.
        assert_eq!(tidy("already tidy text"), "already tidy text");
    }
}
