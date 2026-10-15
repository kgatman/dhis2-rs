//! Port of `org.hisp.dhis.common.PropertyPath` (dhis-api).
//!
//! A dot-separated path of property names. A segment may start with a modifier: `:` (preset,
//! e.g. `:all`, `:owner`) or `!`/`-` (exclude). Invariants enforced on construction, with Java's
//! messages: segments are non-empty `[_a-zA-Z0-9]` (after an optional modifier), at most one
//! exclude per path, and a preset can only be the last segment.

use std::fmt;

use crate::IllegalArgument;

const MAX_LENGTH: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PropertyPath {
    /// Raw segments including modifiers, root first.
    segments: Vec<String>,
}

fn is_path_char(c: char) -> bool {
    c == '_' || c.is_ascii_alphanumeric()
}

fn is_preset_modifier(c: char) -> bool {
    c == ':'
}

fn is_exclude_modifier(c: char) -> bool {
    c == '-' || c == '!'
}

fn is_path_modifier(c: char) -> bool {
    is_preset_modifier(c) || is_exclude_modifier(c)
}

fn illegal_empty() -> IllegalArgument {
    IllegalArgument("A property path cannot be null or empty".into())
}

fn illegal_empty_segment() -> IllegalArgument {
    IllegalArgument("A property path segment cannot be empty".into())
}

fn illegal_character(c: char) -> IllegalArgument {
    IllegalArgument(format!(
        "A property path contained an illegal character: {c}"
    ))
}

impl PropertyPath {
    /// The canonical constructor's checks for appending `segment` to `parent`.
    fn checked(
        parent: Option<&PropertyPath>,
        segment: &str,
    ) -> Result<PropertyPath, IllegalArgument> {
        let mut chars = segment.chars();
        let Some(c0) = chars.next() else {
            return Err(illegal_empty_segment());
        };
        if !is_path_char(c0) && !is_path_modifier(c0) {
            return Err(illegal_character(c0));
        }
        if segment.chars().count() == 1 && !is_path_char(c0) {
            return Err(illegal_empty_segment());
        }
        if let Some(bad) = chars.find(|c| !is_path_char(*c)) {
            return Err(illegal_character(bad));
        }
        if let Some(p) = parent {
            if is_exclude_modifier(c0) && p.is_exclude() {
                return Err(IllegalArgument(
                    "A property path can only have one exclude or preset modifier".into(),
                ));
            }
            if p.is_preset() {
                return Err(IllegalArgument(
                    "A property path with preset must always feature the preset at the tail".into(),
                ));
            }
        }
        let mut segments = parent.map(|p| p.segments.clone()).unwrap_or_default();
        segments.push(segment.to_string());
        Ok(PropertyPath { segments })
    }

    /// `PropertyPath.of(CharSequence)`.
    pub fn of(path: &str) -> Result<PropertyPath, IllegalArgument> {
        if path.is_empty() {
            return Err(illegal_empty());
        }
        if path == "*" {
            return Self::of(":all");
        }
        let p: Vec<char> = path.chars().collect();
        // Java compares String#length (UTF-16 units).
        if path.encode_utf16().count() > MAX_LENGTH {
            return Err(IllegalArgument(format!(
                "A property path cannot be longer than {MAX_LENGTH} characters"
            )));
        }
        let end = p.len();
        let mut start = 0;
        let mut i = 0;
        let mut res: Option<PropertyPath> = None;
        while i < end {
            if is_path_modifier(p[i]) {
                i += 1;
            }
            while i < end && is_path_char(p[i]) {
                i += 1;
            }
            let seg: String = p[start..i.min(end)].iter().collect();
            if seg.is_empty() {
                return Err(illegal_empty_segment());
            }
            res = Some(Self::checked(res.as_ref(), &seg)?);
            if i < end && p[i] != '.' {
                return Err(illegal_character(p[i]));
            }
            i += 1;
            start = i;
        }
        res.ok_or_else(illegal_empty)
    }

    /// `PropertyPath.concat(parent, property)`.
    pub fn concat_opt(
        parent: Option<&PropertyPath>,
        property: &str,
    ) -> Result<PropertyPath, IllegalArgument> {
        match parent {
            None => Self::of(property),
            Some(p) => p.concat(property),
        }
    }

    /// `concat(CharSequence)`: appends one segment (no splitting on `.`).
    pub fn concat(&self, segment: &str) -> Result<PropertyPath, IllegalArgument> {
        if segment == "*" {
            return self.concat(":all");
        }
        Self::checked(Some(self), segment)
    }

    pub fn segments(&self) -> &[String] {
        &self.segments
    }

    pub fn len(&self) -> usize {
        self.segments.len()
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    pub fn is_nested(&self) -> bool {
        self.segments.len() > 1
    }

    fn tail(&self) -> &str {
        self.segments.last().map(String::as_str).unwrap_or("")
    }

    /// `isExclude()`: any segment starts with an exclude modifier.
    pub fn is_exclude(&self) -> bool {
        self.segments
            .iter()
            .any(|s| s.starts_with(is_exclude_modifier))
    }

    /// `isPreset()`: the tail starts with `:`.
    pub fn is_preset(&self) -> bool {
        self.tail().starts_with(':')
    }

    /// `isAll()`.
    pub fn is_all(&self) -> bool {
        self.tail() == ":all"
    }

    /// `property()`: the tail without an exclude modifier.
    pub fn property(&self) -> &str {
        let t = self.tail();
        if t.starts_with(is_exclude_modifier) {
            &t[1..]
        } else {
            t
        }
    }

    /// `parent`.
    pub fn parent(&self) -> Option<PropertyPath> {
        (self.segments.len() > 1).then(|| PropertyPath {
            segments: self.segments[..self.segments.len() - 1].to_vec(),
        })
    }
}

impl fmt::Display for PropertyPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.segments.join("."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PropertyPath {
        PropertyPath::of(s).unwrap()
    }

    fn err(s: &str) -> String {
        PropertyPath::of(s).unwrap_err().0
    }

    #[test]
    fn parsing() {
        assert_eq!(p("a.b.c").segments(), ["a", "b", "c"]);
        assert_eq!(p("*").to_string(), ":all");
        assert_eq!(p("a.!b").to_string(), "a.!b");
        assert!(p("a.!b").is_exclude());
        assert!(p("a.:owner").is_preset());
        assert_eq!(p("!code").property(), "code");
    }

    #[test]
    fn errors_match_java() {
        assert_eq!(err(""), "A property path cannot be null or empty");
        assert_eq!(err("a..b"), "A property path segment cannot be empty");
        assert_eq!(
            err("a$"),
            "A property path contained an illegal character: $"
        );
        assert_eq!(err("!"), "A property path segment cannot be empty");
        assert_eq!(
            err("!a.!b"),
            "A property path can only have one exclude or preset modifier"
        );
        assert_eq!(
            err(":all.x"),
            "A property path with preset must always feature the preset at the tail"
        );
        assert_eq!(
            p("a").concat("b-c").unwrap_err().0,
            "A property path contained an illegal character: -"
        );
    }
}
