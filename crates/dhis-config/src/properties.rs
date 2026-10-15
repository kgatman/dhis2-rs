//! Port of `java.util.Properties#load(InputStream)` (JDK 17, `Properties.LineReader` and
//! `Properties.loadConvert`).
//!
//! Java's `PropertiesLoaderUtils.loadProperties(Resource)` (used by
//! `DefaultDhisConfigurationProvider.loadDhisConf`) calls `Properties.load(InputStream)`, which
//! decodes bytes as ISO-8859-1. Non-ASCII bytes in `dhis.conf` therefore become Latin-1 chars and
//! anything else must be written as `\uXXXX`. We mirror that exactly.

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PropertiesError {
    /// `IllegalArgumentException("Malformed \\uxxxx encoding.")` in Java.
    #[error("Malformed \\uxxxx encoding.")]
    MalformedUnicodeEscape,
}

/// Parses a properties document and returns `(key, value)` pairs in file order.
///
/// Duplicate keys appear once per occurrence; callers that insert them into a map in order get
/// Java's "last one wins" behaviour.
pub fn parse(bytes: &[u8]) -> Result<Vec<(String, String)>, PropertiesError> {
    let mut reader = LineReader::new(bytes);
    let mut out = Vec::new();

    while let Some(line) = reader.read_line() {
        let limit = line.len();
        let mut key_len = 0;
        let mut value_start = limit;
        let mut has_sep = false;
        let mut preceding_backslash = false;

        while key_len < limit {
            let c = line[key_len];
            if (c == '=' || c == ':') && !preceding_backslash {
                value_start = key_len + 1;
                has_sep = true;
                break;
            } else if (c == ' ' || c == '\t' || c == '\x0c') && !preceding_backslash {
                value_start = key_len + 1;
                break;
            }
            if c == '\\' {
                preceding_backslash = !preceding_backslash;
            } else {
                preceding_backslash = false;
            }
            key_len += 1;
        }

        while value_start < limit {
            let c = line[value_start];
            if c != ' ' && c != '\t' && c != '\x0c' {
                if !has_sep && (c == '=' || c == ':') {
                    has_sep = true;
                } else {
                    break;
                }
            }
            value_start += 1;
        }

        let key = load_convert(&line[..key_len])?;
        let value = load_convert(&line[value_start..limit])?;
        out.push((key, value));
    }

    Ok(out)
}

/// `Properties.loadConvert`: resolves `\uXXXX`, `\t`, `\r`, `\n`, `\f` and drops the backslash in
/// front of any other char.
///
/// Java strings are UTF-16, so `\uD83D\uDE00` pairs combine into one code point. A lone surrogate
/// cannot exist in a Rust `String` and is replaced by U+FFFD (see docs/deviations.md).
fn load_convert(input: &[char]) -> Result<String, PropertiesError> {
    let mut out: Vec<u16> = Vec::with_capacity(input.len());
    let mut off = 0;
    let end = input.len();

    while off < end {
        let mut c = input[off];
        off += 1;
        if c == '\\' {
            // A lone trailing backslash cannot reach here: LineReader drops it. Guard anyway.
            if off >= end {
                break;
            }
            c = input[off];
            off += 1;
            if c == 'u' {
                if off + 4 > end {
                    return Err(PropertiesError::MalformedUnicodeEscape);
                }
                let mut value: u16 = 0;
                for _ in 0..4 {
                    // Input chars are Latin-1 (<= U+00FF), where Character.digit(c, 16) accepts
                    // exactly [0-9a-fA-F].
                    let digit = input[off]
                        .to_digit(16)
                        .ok_or(PropertiesError::MalformedUnicodeEscape)?;
                    off += 1;
                    value = (value << 4) + digit as u16;
                }
                out.push(value);
            } else {
                let c = match c {
                    't' => '\t',
                    'r' => '\r',
                    'n' => '\n',
                    'f' => '\x0c',
                    other => other,
                };
                out.push(c as u16);
            }
        } else {
            // Latin-1 chars always fit in one UTF-16 unit.
            out.push(c as u16);
        }
    }

    Ok(String::from_utf16_lossy(&out))
}

/// Port of `Properties.LineReader#readLine` for an `InputStream` source.
struct LineReader<'a> {
    input: &'a [u8],
    off: usize,
    skip_lf: bool,
}

impl<'a> LineReader<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self {
            input,
            off: 0,
            skip_lf: false,
        }
    }

    fn next_char(&mut self) -> Option<char> {
        let b = *self.input.get(self.off)?;
        self.off += 1;
        // (char)(byte & 0xFF) == ISO-8859-1 decoding.
        Some(b as char)
    }

    /// Returns the next logical line, or `None` at end of input.
    fn read_line(&mut self) -> Option<Vec<char>> {
        let mut line: Vec<char> = Vec::new();
        let mut skip_white_space = true;
        let mut appended_line_begin = false;
        let mut preceding_backslash = false;

        loop {
            let Some(c) = self.next_char() else {
                if line.is_empty() {
                    return None;
                }
                if preceding_backslash {
                    line.pop();
                }
                return Some(line);
            };

            if self.skip_lf {
                self.skip_lf = false;
                if c == '\n' {
                    continue;
                }
            }

            if skip_white_space {
                if c == ' ' || c == '\t' || c == '\x0c' {
                    continue;
                }
                if !appended_line_begin && (c == '\r' || c == '\n') {
                    continue;
                }
                skip_white_space = false;
                appended_line_begin = false;
            }

            if line.is_empty() && (c == '#' || c == '!') {
                // Comment: consume the rest of the natural line.
                loop {
                    match self.next_char() {
                        None => break,
                        Some('\n') => break,
                        Some('\r') => {
                            self.skip_lf = true;
                            break;
                        }
                        Some(_) => {}
                    }
                }
                skip_white_space = true;
                continue;
            }

            if c != '\n' && c != '\r' {
                line.push(c);
                if c == '\\' {
                    preceding_backslash = !preceding_backslash;
                } else {
                    preceding_backslash = false;
                }
            } else {
                // Reached end of a natural line.
                if line.is_empty() {
                    skip_white_space = true;
                    continue;
                }
                if self.off >= self.input.len() {
                    if preceding_backslash {
                        line.pop();
                    }
                    return Some(line);
                }
                if preceding_backslash {
                    // The backslash at EOL is not part of the line; join the next natural line.
                    line.pop();
                    skip_white_space = true;
                    appended_line_begin = true;
                    preceding_backslash = false;
                    if c == '\r' {
                        self.skip_lf = true;
                    }
                } else {
                    return Some(line);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Vec<(String, String)> {
        parse(s.as_bytes()).unwrap()
    }

    fn kv(k: &str, v: &str) -> (String, String) {
        (k.to_string(), v.to_string())
    }

    #[test]
    fn separators() {
        assert_eq!(
            p("a=1\nb:2\nc 3\nd = 4\ne\t:\t5\nf==6\ng\n"),
            vec![
                kv("a", "1"),
                kv("b", "2"),
                kv("c", "3"),
                kv("d", "4"),
                kv("e", "5"),
                kv("f", "=6"),
                kv("g", "")
            ]
        );
    }

    #[test]
    fn comments_and_blank_lines() {
        assert_eq!(
            p("# c\n  ! c2\n\n   \nkey = value # not a comment\n"),
            vec![kv("key", "value # not a comment")]
        );
    }

    #[test]
    fn trailing_whitespace_is_kept_in_value() {
        // Properties keeps it; DefaultDhisConfigurationProvider trims afterwards.
        assert_eq!(p("a = 1  \n"), vec![kv("a", "1  ")]);
    }

    #[test]
    fn continuation_lines() {
        assert_eq!(
            p("a = one, \\\n    two, \\\r\n\tthree\nb=x"),
            vec![kv("a", "one, two, three"), kv("b", "x")]
        );
        // Even number of backslashes is not a continuation.
        assert_eq!(p("a=x\\\\\nb=y"), vec![kv("a", "x\\"), kv("b", "y")]);
        // Trailing odd backslash at EOF is dropped.
        assert_eq!(p("a=x\\"), vec![kv("a", "x")]);
    }

    #[test]
    fn escapes() {
        assert_eq!(
            p("k\\=ey\\ x = a\\tb\\nc\\u0041\\q\n"),
            vec![kv("k=ey x", "a\tb\ncAq")]
        );
        assert_eq!(p("e=\\uD83D\\uDE00"), vec![kv("e", "😀")]);
        assert_eq!(
            parse(b"a=\\u12"),
            Err(PropertiesError::MalformedUnicodeEscape)
        );
        assert_eq!(
            parse(b"a=\\u12zz"),
            Err(PropertiesError::MalformedUnicodeEscape)
        );
    }

    #[test]
    fn latin1_decoding() {
        // 0xE9 is 'é' in ISO-8859-1; a UTF-8 'é' (C3 A9) becomes two chars, as in Java.
        assert_eq!(parse(b"a=\xe9").unwrap(), vec![kv("a", "é")]);
        assert_eq!(parse("a=é".as_bytes()).unwrap(), vec![kv("a", "Ã©")]);
    }

    #[test]
    fn cr_only_line_endings() {
        assert_eq!(p("a=1\rb=2\r"), vec![kv("a", "1"), kv("b", "2")]);
    }
}
