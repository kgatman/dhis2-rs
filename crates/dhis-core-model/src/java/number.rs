//! Java/Spring integer parsing, as used when Spring MVC binds an `Integer` request parameter.
//!
//! Chain (Spring 7.0.9, JDK 17–27, all verified against the real JDK with jshell; fixtures in
//! the tests below were produced by the JDK):
//!
//! 1. `CustomNumberEditor(allowEmpty=true)`: text without any non-whitespace char → `null`
//!    (the `@RequestParam` default then applies);
//! 2. `NumberUtils.parseNumber`: `StringUtils.trimAllWhitespace` (removes *all* whitespace, also
//!    inside: `"1 2"` → `12`), then `Integer.decode` if the text is hex (`0x`, `0X`, `#` after an
//!    optional `-`), else `Integer.valueOf`;
//! 3. `Integer.parseInt` accepts any BMP char with a `Character.digit` value (Arabic-Indic,
//!    fullwidth, ... digits; fullwidth A–F for hex).

use super::digit_table::DECIMAL_RUNS;

/// Java's `NumberFormatException` message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumberFormatException(pub String);

impl NumberFormatException {
    /// `NumberFormatException.forInputString(s, radix)`.
    fn for_input(s: &[u16], radix: u32) -> Self {
        let s = String::from_utf16_lossy(s);
        if radix == 10 {
            Self(format!("For input string: \"{s}\""))
        } else {
            Self(format!("For input string: \"{s}\" under radix {radix}"))
        }
    }
}

/// `Character.isWhitespace(char)`.
pub fn is_java_whitespace(c: u16) -> bool {
    matches!(c, 0x09..=0x0D | 0x1C..=0x1F | 0x20 | 0x1680 | 0x2000..=0x2006 | 0x2008..=0x200A
        | 0x2028 | 0x2029 | 0x205F | 0x3000)
}

/// `Character.digit(char, radix)` for radix 10 and 16.
pub fn java_digit(c: u16, radix: u32) -> Option<u32> {
    let c32 = u32::from(c);
    for &(first, last, base) in DECIMAL_RUNS {
        if (first..=last).contains(&c32) {
            return Some(base + c32 - first);
        }
    }
    if radix == 16 {
        for first in [0x41, 0x61, 0xFF21, 0xFF41] {
            if (first..first + 6).contains(&c32) {
                return Some(10 + c32 - first);
            }
        }
    }
    None
}

/// `Integer.parseInt(String, radix)` on UTF-16 units (supplementary digits are not digits).
fn parse_int(s: &[u16], radix: u32) -> Result<i32, NumberFormatException> {
    let err = || NumberFormatException::for_input(s, radix);
    if s.is_empty() {
        return Err(err());
    }
    let (negative, digits) = match s[0] {
        0x2D => (true, &s[1..]),
        0x2B => (false, &s[1..]),
        c if c < 0x30 => return Err(err()),
        _ => (false, s),
    };
    if digits.is_empty() {
        return Err(err());
    }
    // Accumulate negatively like Java, so i32::MIN works.
    let limit: i64 = if negative {
        i64::from(i32::MIN)
    } else {
        -i64::from(i32::MAX)
    };
    let mut result: i64 = 0;
    for &c in digits {
        let d = java_digit(c, radix)
            .filter(|d| *d < radix)
            .ok_or_else(err)?;
        result = result * i64::from(radix) - i64::from(d);
        if result < limit {
            return Err(err());
        }
    }
    Ok(if negative {
        result as i32
    } else {
        (-result) as i32
    })
}

/// `Integer.decode(String)`.
fn decode(nm: &[u16]) -> Result<i32, NumberFormatException> {
    if nm.is_empty() {
        return Err(NumberFormatException("Zero length string".into()));
    }
    let starts = |i: usize, p: &str| {
        let p: Vec<u16> = p.encode_utf16().collect();
        nm.len() >= i + p.len() && nm[i..i + p.len()] == p[..]
    };
    let mut index = 0;
    let mut negative = false;
    match nm[0] {
        0x2D => {
            negative = true;
            index += 1;
        }
        0x2B => index += 1,
        _ => {}
    }
    let mut radix = 10;
    if starts(index, "0x") || starts(index, "0X") {
        index += 2;
        radix = 16;
    } else if starts(index, "#") {
        index += 1;
        radix = 16;
    } else if starts(index, "0") && nm.len() > 1 + index {
        index += 1;
        radix = 8;
    }
    if starts(index, "-") || starts(index, "+") {
        return Err(NumberFormatException(
            "Sign character in wrong position".into(),
        ));
    }
    match parse_int(&nm[index..], radix) {
        Ok(v) => Ok(if negative { v.wrapping_neg() } else { v }),
        Err(_) => {
            // Java retries with the sign attached (handles MIN_VALUE); its error is the one seen.
            let mut constant: Vec<u16> = Vec::new();
            if negative {
                constant.push(0x2D);
            }
            constant.extend_from_slice(&nm[index..]);
            parse_int(&constant, radix)
        }
    }
}

/// `Integer.valueOf(String)` (radix 10, no trimming).
pub fn integer_value_of(s: &str) -> Result<i32, NumberFormatException> {
    let units: Vec<u16> = s.encode_utf16().collect();
    parse_int(&units, 10)
}

/// Binds an `Integer` request parameter like Spring MVC. `Ok(None)` means "no value"
/// (the parameter's `defaultValue` applies).
pub fn spring_integer(text: &str) -> Result<Option<i32>, NumberFormatException> {
    let units: Vec<u16> = text.encode_utf16().collect();
    // CustomNumberEditor: !StringUtils.hasText(text) → null.
    if units.iter().all(|c| is_java_whitespace(*c)) {
        return Ok(None);
    }
    let trimmed: Vec<u16> = units
        .into_iter()
        .filter(|c| !is_java_whitespace(*c))
        .collect();
    let body = if trimmed.first() == Some(&0x2D) {
        &trimmed[1..]
    } else {
        &trimmed[..]
    };
    let is_hex = body.starts_with(&[0x30, 0x78])
        || body.starts_with(&[0x30, 0x58])
        || body.starts_with(&[0x23]);
    if is_hex {
        decode(&trimmed).map(Some)
    } else {
        parse_int(&trimmed, 10).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Output of `Integer.valueOf` / `Integer.decode` on JDK 17 and JDK 27 (identical), with
    /// Spring's hex detection. Generated by jshell, see PORT_STATUS session notes.
    const JDK: &[(&str, Result<i32, &str>)] = &[
        ("3", Ok(3)),
        ("+3", Ok(3)),
        ("-3", Ok(-3)),
        ("0", Ok(0)),
        ("-0", Ok(0)),
        ("010", Ok(10)),
        ("0x10", Ok(16)),
        ("0X1f", Ok(31)),
        ("#10", Ok(16)),
        ("-0x2", Ok(-2)),
        ("-#2", Ok(-2)),
        ("0x", Err("For input string: \"\" under radix 16")),
        ("#", Err("For input string: \"\" under radix 16")),
        ("-", Err("For input string: \"-\"")),
        ("+", Err("For input string: \"+\"")),
        ("1.5", Err("For input string: \"1.5\"")),
        ("abc", Err("For input string: \"abc\"")),
        ("99999999999", Err("For input string: \"99999999999\"")),
        ("2147483647", Ok(2147483647)),
        ("2147483648", Err("For input string: \"2147483648\"")),
        ("-2147483648", Ok(-2147483648)),
        ("-2147483649", Err("For input string: \"-2147483649\"")),
        ("0x7fffffff", Ok(2147483647)),
        (
            "0x80000000",
            Err("For input string: \"80000000\" under radix 16"),
        ),
        ("-0x80000000", Ok(-2147483648)),
        ("0xZZ", Err("For input string: \"ZZ\" under radix 16")),
        ("-0xZZ", Err("For input string: \"-ZZ\" under radix 16")),
        ("0x-1", Err("Sign character in wrong position")),
        ("0x+1", Err("Sign character in wrong position")),
        ("+0x3", Err("For input string: \"+0x3\"")),
        ("\u{663}", Ok(3)),
        ("\u{FF13}", Ok(3)),
        ("\u{FF11}\u{FF12}", Ok(12)),
        ("0x\u{FF26}", Ok(15)),
        ("\u{663}\u{663}\u{663}", Ok(333)),
        ("1_000", Err("For input string: \"1_000\"")),
        ("1e3", Err("For input string: \"1e3\"")),
    ];

    #[test]
    fn matches_jdk() {
        for (input, expected) in JDK {
            let got = spring_integer(input).map(|v| v.expect("non-empty"));
            let expected = expected.map_err(|m| NumberFormatException(m.to_string()));
            assert_eq!(got, expected, "{input:?}");
        }
    }

    /// Measured against the Java server (`GET /api/system/uid?limit=...`).
    #[test]
    fn spring_whitespace_handling() {
        assert_eq!(spring_integer(""), Ok(None));
        assert_eq!(spring_integer(" "), Ok(None));
        assert_eq!(spring_integer("\t"), Ok(None));
        assert_eq!(spring_integer("\u{2003}"), Ok(None));
        assert_eq!(spring_integer(" 3"), Ok(Some(3)));
        assert_eq!(spring_integer("1\t2"), Ok(Some(12)));
        assert_eq!(spring_integer("1 2"), Ok(Some(12)));
        // NBSP is not Java whitespace.
        assert_eq!(
            spring_integer("\u{A0}5"),
            Err(NumberFormatException(
                "For input string: \"\u{A0}5\"".into()
            ))
        );
    }
}
