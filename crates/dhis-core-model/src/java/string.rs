//! `java.lang.String` helpers whose semantics differ from Rust's.

/// `String.trim()`: strips leading and trailing chars `<= U+0020` (all ASCII controls and space,
/// but not DEL or Unicode whitespace).
pub fn trim(s: &str) -> &str {
    s.trim_matches(|c: char| c <= ' ')
}

#[cfg(test)]
mod tests {
    #[test]
    fn trim_like_java() {
        assert_eq!(super::trim(" \u{1}a b\t\n"), "a b");
        assert_eq!(super::trim("\u{7f}a\u{a0}"), "\u{7f}a\u{a0}");
        assert_eq!(super::trim("   "), "");
    }
}
