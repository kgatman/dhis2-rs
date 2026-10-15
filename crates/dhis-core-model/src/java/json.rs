//! JSON text exactly as Jackson writes it (the default `ObjectMapper` used by the Web API).
//!
//! Strings: `"` and `\` are escaped, `\b \t \n \f \r` use their short forms, other control
//! characters `\u00XX` with **upper-case** hex (serde_json writes lower-case); everything else,
//! including `/`, DEL, U+2028 and non-ASCII, is written as is (measured on Java).
//! Doubles: `Double.toString` (JDK 21).

use std::io;

use serde::Serialize;
use serde_json::ser::{CharEscape, Formatter};

/// serde_json's compact output with Jackson's escapes and `Double.toString` for doubles.
#[derive(Debug, Clone, Copy, Default)]
pub struct JacksonFormatter;

impl Formatter for JacksonFormatter {
    fn write_char_escape<W: ?Sized + io::Write>(
        &mut self,
        writer: &mut W,
        char_escape: CharEscape,
    ) -> io::Result<()> {
        match char_escape {
            CharEscape::Quote => writer.write_all(b"\\\""),
            CharEscape::ReverseSolidus => writer.write_all(b"\\\\"),
            CharEscape::Solidus => writer.write_all(b"\\/"),
            CharEscape::Backspace => writer.write_all(b"\\b"),
            CharEscape::FormFeed => writer.write_all(b"\\f"),
            CharEscape::LineFeed => writer.write_all(b"\\n"),
            CharEscape::CarriageReturn => writer.write_all(b"\\r"),
            CharEscape::Tab => writer.write_all(b"\\t"),
            CharEscape::AsciiControl(b) => write!(writer, "\\u{b:04X}"),
        }
    }

    fn write_f64<W: ?Sized + io::Write>(&mut self, writer: &mut W, value: f64) -> io::Result<()> {
        writer.write_all(double_to_string(value).as_bytes())
    }

    fn write_f32<W: ?Sized + io::Write>(&mut self, _writer: &mut W, _value: f32) -> io::Result<()> {
        // Float.toString is not ported; no response type has a float field.
        Err(io::Error::other("f32 output is not supported"))
    }
}

/// Compact JSON, as Jackson writes it. Struct fields and map entries keep their serde order.
pub fn to_vec<T: ?Sized + Serialize>(value: &T) -> Vec<u8> {
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut out, JacksonFormatter);
    value
        .serialize(&mut ser)
        .expect("serialising to memory; f32 is not used");
    out
}

pub fn to_string<T: ?Sized + Serialize>(value: &T) -> String {
    String::from_utf8(to_vec(value)).expect("serde_json writes UTF-8")
}

/// A JSON string literal, Jackson style.
pub fn string_literal(s: &str) -> String {
    to_string(s)
}

/// Appends a JSON string literal, Jackson style.
pub fn write_string(out: &mut String, s: &str) {
    out.push_str(&string_literal(s));
}

/// `Double.toString(d)` as of JDK 19+: the shortest decimal that rounds to `d` (closest one if
/// several; with two digits when one would do), plain for `1e-3 <= |d| < 1e7`, else
/// `d.dddE±n`.
pub fn double_to_string(d: f64) -> String {
    if d.is_nan() {
        return "NaN".into();
    }
    if d.is_infinite() {
        return if d > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if d == 0.0 {
        return if d.is_sign_negative() { "-0.0" } else { "0.0" }.into();
    }
    let abs = d.abs();
    let sci = |s: String| -> (String, i32) {
        let (mant, exp) = s.split_once('e').expect("{:e} output");
        (mant.replace('.', ""), exp.parse().expect("exponent"))
    };
    // Rust prints the shortest round-tripping digits, picking the closest when there are several.
    let (mut digits, mut exp) = sci(format!("{abs:e}"));
    if digits.len() == 1 {
        // Java chooses among two-digit decimals then: the correctly rounded one.
        (digits, exp) = sci(format!("{abs:.1e}"));
        while digits.len() > 1 && digits.ends_with('0') {
            digits.pop();
        }
    }
    let mut out = String::new();
    if d < 0.0 {
        out.push('-');
    }
    if (1e-3..1e7).contains(&abs) {
        if exp >= 0 {
            let int_len = exp as usize + 1;
            if digits.len() <= int_len {
                out.push_str(&digits);
                out.push_str(&"0".repeat(int_len - digits.len()));
                out.push_str(".0");
            } else {
                out.push_str(&digits[..int_len]);
                out.push('.');
                out.push_str(&digits[int_len..]);
            }
        } else {
            out.push_str("0.");
            out.push_str(&"0".repeat((-exp - 1) as usize));
            out.push_str(&digits);
        }
    } else {
        out.push_str(&digits[..1]);
        out.push('.');
        out.push_str(if digits.len() > 1 { &digits[1..] } else { "0" });
        out.push('E');
        out.push_str(&exp.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Double.toString` outputs printed by the server image's JDK 21 (bits, text).
    #[test]
    fn doubles_like_jdk21() {
        for (bits, want) in [
            (0i64, "0.0"),
            (-9223372036854775808, "-0.0"),
            (4607182418800017408, "1.0"),
            (-4613937818241073152, "-1.5"),
            (4591870180066957722, "0.1"),
            (4711630319722168320, "1.0E7"),
            (4711630319185297408, "9999999.0"),
            (4711630319453732864, "9999999.5"),
            (4562254508917369340, "0.001"),
            (4562208392057185066, "9.9E-4"),
            (4547007122018943789, "1.0E-4"),
            (4638387860618067575, "123.456"),
            (4921056587992461136, "1.0E21"),
            (4955416454957714166, "2.0E23"),
            (9218868437227405311, "1.7976931348623157E308"),
            (1, "4.9E-324"),
            (4636737291354636288, "100.0"),
            (4698053236609777664, "1234567.0"),
            (4712889646009286656, "1.2345678E7"),
            (4599075939470750515, "0.3"),
            (4599676419421066581, "0.3333333333333333"),
            (4604180019048437077, "0.6666666666666666"),
            (4502148214488346440, "1.0E-7"),
            (4772899269882675200, "1.23456789012E11"),
            (4464640349921820037, "3.14159E-10"),
        ] {
            assert_eq!(double_to_string(f64::from_bits(bits as u64)), want);
        }
    }

    #[test]
    fn values_like_jackson() {
        let v = serde_json::json!({"b": [1, -2, 1.5e-7, null, true], "a": "\u{1}"});
        assert_eq!(
            to_string(&v),
            r#"{"a":"\u0001","b":[1,-2,1.5E-7,null,true]}"#
        );
    }

    /// Measured: Java renders the setting value `a\x1fb\x0bc\x7f\u{2028}d</` and `\b\f\t`.
    #[test]
    fn strings_like_jackson() {
        assert_eq!(
            string_literal("a\u{1f}b\u{b}c\u{7f}\u{2028}d</"),
            "\"a\\u001Fb\\u000Bc\u{7f}\u{2028}d</\""
        );
        assert_eq!(string_literal("\u{8}\u{c}\t"), "\"\\b\\f\\t\"");
        assert_eq!(string_literal("q\"\\\n\r\0"), "\"q\\\"\\\\\\n\\r\\u0000\"");
    }
}
