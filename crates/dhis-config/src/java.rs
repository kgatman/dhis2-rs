//! Tiny ports of the JDK / commons-lang3 helpers the configuration code relies on, so that values
//! are interpreted exactly as Java interprets them.

/// `String#trim()`: strips leading/trailing chars `<= U+0020` (not Unicode whitespace).
pub fn trim(s: &str) -> &str {
    s.trim_matches(|c: char| c <= ' ')
}

/// `Character#isWhitespace(char)`.
pub fn is_whitespace(c: char) -> bool {
    match c {
        '\u{00A0}' | '\u{2007}' | '\u{202F}' => false,
        '\t' | '\n' | '\u{000B}' | '\u{000C}' | '\r' | '\u{001C}'..='\u{001F}' => true,
        // SPACE_SEPARATOR, LINE_SEPARATOR, PARAGRAPH_SEPARATOR minus the no-break spaces above.
        ' '
        | '\u{1680}'
        | '\u{2000}'..='\u{200A}'
        | '\u{2028}'
        | '\u{2029}'
        | '\u{205F}'
        | '\u{3000}' => true,
        _ => false,
    }
}

/// `String#strip()`.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_whitespace)
}

/// `DhisConfigurationProvider.isOn`: `Boolean.parseBoolean(v) || "on".equalsIgnoreCase(v)`.
pub fn is_on(value: Option<&str>) -> bool {
    value.is_some_and(|v| v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("on"))
}

/// Java threw `NumberFormatException` where we return this.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("For input string: \"{0}\"")]
pub struct NumberFormatException(pub String);

/// `DhisConfigurationProvider.toInt`:
/// `NumberUtils.isParsable(value) ? Integer.parseInt(value) : -1`.
///
/// `isParsable` accepts anything `Double.parseDouble` or `Long.parseLong` accepts, so values such
/// as `"1.5"` or `" 7"` pass the check and then make `Integer.parseInt` throw. We keep that.
pub fn to_int(value: Option<&str>) -> Result<i32, NumberFormatException> {
    match value {
        Some(v) if is_parsable(v) => {
            parse_int(v).ok_or_else(|| NumberFormatException(v.to_string()))
        }
        _ => Ok(-1),
    }
}

/// commons-lang3 3.21.0 `NumberUtils.isParsable`.
pub fn is_parsable(s: &str) -> bool {
    is_java_double(s) || parse_long(s).is_some()
}

/// `Integer.parseInt(s, 10)`, ASCII digits only (see docs/deviations.md).
pub fn parse_int(s: &str) -> Option<i32> {
    parse_signed(s).and_then(|v| i32::try_from(v).ok())
}

/// `Long.parseLong(s, 10)`, ASCII digits only (see docs/deviations.md).
pub fn parse_long(s: &str) -> Option<i64> {
    parse_signed(s).and_then(|v| i64::try_from(v).ok())
}

fn parse_signed(s: &str) -> Option<i128> {
    let (neg, digits) = match s.as_bytes().first()? {
        b'-' => (true, &s[1..]),
        b'+' => (false, &s[1..]),
        _ => (false, s),
    };
    if digits.is_empty() || digits.len() > 30 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let v: i128 = digits.parse().ok()?;
    Some(if neg { -v } else { v })
}

/// Whether `Double.parseDouble(s)` succeeds (`jdk.internal.math.FloatingDecimal
/// #readJavaFormatString`).
pub fn is_java_double(s: &str) -> bool {
    let s = trim(s).as_bytes();
    let mut i = 0;
    if matches!(s.first(), Some(b'+' | b'-')) {
        i += 1;
    }
    let rest = &s[i..];
    if rest == b"NaN" || rest == b"Infinity" {
        return true;
    }
    if rest.len() >= 2 && rest[0] == b'0' && (rest[1] == b'x' || rest[1] == b'X') {
        return is_hex_float(&rest[2..]);
    }

    let digits_before = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    let mut j = digits_before;
    let mut digits_after = 0;
    if rest.get(j) == Some(&b'.') {
        j += 1;
        digits_after = rest[j..].iter().take_while(|b| b.is_ascii_digit()).count();
        j += digits_after;
    }
    if digits_before + digits_after == 0 {
        return false;
    }
    if matches!(rest.get(j), Some(b'e' | b'E')) {
        j += 1;
        if matches!(rest.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        let exp_digits = rest[j..].iter().take_while(|b| b.is_ascii_digit()).count();
        if exp_digits == 0 {
            return false;
        }
        j += exp_digits;
    }
    if matches!(rest.get(j), Some(b'f' | b'F' | b'd' | b'D')) {
        j += 1;
    }
    j == rest.len()
}

/// Hex part after `0x`: `((XDigit+)\.?)|((XDigit*)\.(XDigit+))` `[pP][+-]?Digit+` `[fFdD]?`.
fn is_hex_float(s: &[u8]) -> bool {
    let before = s.iter().take_while(|b| b.is_ascii_hexdigit()).count();
    let mut j = before;
    let mut after = 0;
    if s.get(j) == Some(&b'.') {
        j += 1;
        after = s[j..].iter().take_while(|b| b.is_ascii_hexdigit()).count();
        j += after;
    }
    if before + after == 0 {
        return false;
    }
    if !matches!(s.get(j), Some(b'p' | b'P')) {
        return false;
    }
    j += 1;
    if matches!(s.get(j), Some(b'+' | b'-')) {
        j += 1;
    }
    let exp = s[j..].iter().take_while(|b| b.is_ascii_digit()).count();
    if exp == 0 {
        return false;
    }
    j += exp;
    if matches!(s.get(j), Some(b'f' | b'F' | b'd' | b'D')) {
        j += 1;
    }
    j == s.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_on_matches_java() {
        for v in ["on", "ON", "On", "true", "TRUE", "tRuE"] {
            assert!(is_on(Some(v)), "{v}");
        }
        for v in ["off", "yes", "1", " on", "on ", ""] {
            assert!(!is_on(Some(v)), "{v}");
        }
        assert!(!is_on(None));
    }

    #[test]
    fn to_int_matches_java() {
        assert_eq!(to_int(Some("80")), Ok(80));
        assert_eq!(to_int(Some("+80")), Ok(80));
        assert_eq!(to_int(Some("-3")), Ok(-3));
        assert_eq!(to_int(Some("abc")), Ok(-1));
        assert_eq!(to_int(Some("")), Ok(-1));
        assert_eq!(to_int(None), Ok(-1));
        // isParsable says yes, Integer.parseInt throws.
        assert!(to_int(Some("1.5")).is_err());
        assert!(to_int(Some(" 7")).is_err());
        assert!(to_int(Some("1e3")).is_err());
        assert!(to_int(Some("NaN")).is_err());
        assert!(to_int(Some("4294967296")).is_err());
    }

    #[test]
    fn java_double_grammar() {
        for v in [
            "1",
            "1.",
            ".5",
            "1e5",
            "1E-5",
            "1.5f",
            "2D",
            " 3 ",
            "-Infinity",
            "NaN",
            "0x1p3",
            "0x.8p1",
            "0X1.8P+1d",
        ] {
            assert!(is_java_double(v), "{v}");
        }
        for v in ["", ".", "e5", "1e", "0x1", "inf", "1.5ff", "1 2"] {
            assert!(!is_java_double(v), "{v}");
        }
    }

    #[test]
    fn trim_vs_strip() {
        assert_eq!(trim("\u{1} a \t"), "a");
        assert_eq!(trim("\u{2003}a"), "\u{2003}a");
        assert_eq!(strip("\u{2003}a "), "a");
        assert_eq!(strip("\u{00A0}a"), "\u{00A0}a");
    }
}
