//! Port of `org.apache.commons.text.StringSubstitutor` (commons-text 1.15.0) as constructed by
//! `new StringSubstitutor(System.getenv())` in `DefaultDhisConfigurationProvider`.
//!
//! Defaults in effect: prefix `${`, suffix `}`, escape char `$`, value delimiter `:-`,
//! substitution in variable names disabled, substitution in values enabled (recursive),
//! escapes not preserved, undefined variables left as-is.
//!
//! The algorithm is a line-by-line port of `StringSubstitutor#substitute(TextStringBuilder, int,
//! int, List)` so that edge cases (escapes, unterminated references, cycles) behave the same.

use std::collections::HashMap;

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SubstitutionError {
    /// `IllegalStateException` thrown by `checkCyclicSubstitution`.
    #[error("{0}")]
    InfiniteLoop(String),
}

const PREFIX: [char; 2] = ['$', '{'];
const SUFFIX: char = '}';
const ESCAPE: char = '$';
const VALUE_DELIMITER: [char; 2] = [':', '-'];

/// `StringSubstitutor#replace(String)`.
pub fn replace(source: &str, vars: &HashMap<String, String>) -> Result<String, SubstitutionError> {
    let mut buf: Vec<char> = source.chars().collect();
    let len = buf.len();
    // Java passes the same List instance down the recursion; a shared &mut models that.
    let mut priors: Option<Vec<String>> = None;
    let altered = substitute(&mut buf, 0, len, &mut priors, vars)?.0;
    if !altered {
        return Ok(source.to_string());
    }
    Ok(buf.into_iter().collect())
}

fn starts_with_at(buf: &[char], pos: usize, end: usize, pat: &[char]) -> bool {
    pos + pat.len() <= end && buf[pos..pos + pat.len()] == *pat
}

/// Returns `(altered, length_change)`.
fn substitute(
    builder: &mut Vec<char>,
    offset: usize,
    length: usize,
    priors: &mut Option<Vec<String>>,
    vars: &HashMap<String, String>,
) -> Result<(bool, isize), SubstitutionError> {
    let mut altered = false;
    let mut length_change: isize = 0;
    let mut buf_end = offset + length;
    let mut pos = offset;
    let mut esc_pos: Option<usize> = None;

    'outer: while pos < buf_end {
        if !starts_with_at(builder, pos, buf_end, &PREFIX) {
            pos += 1;
            continue;
        }
        if pos > offset && builder[pos - 1] == ESCAPE {
            esc_pos = Some(pos - 1);
        }
        let start_pos = pos;
        pos += PREFIX.len();

        while pos < buf_end {
            if builder[pos] != SUFFIX {
                pos += 1;
                continue;
            }

            // nestedVarCount is always 0 because substitution in variable names is disabled.
            if let Some(esc) = esc_pos.take() {
                builder.remove(esc);
                length_change -= 1;
                altered = true;
                buf_end -= 1;
                pos = start_pos + 1;
                continue 'outer;
            }

            let expr: Vec<char> = builder[start_pos + PREFIX.len()..pos].to_vec();
            pos += 1;
            let end_pos = pos;

            let mut var_name: String = expr.iter().collect();
            let mut var_default: Option<String> = None;
            for i in 0..expr.len() {
                if starts_with_at(&expr, i, expr.len(), &PREFIX) {
                    break;
                }
                if starts_with_at(&expr, i, expr.len(), &VALUE_DELIMITER) {
                    var_name = expr[..i].iter().collect();
                    var_default = Some(expr[i + VALUE_DELIMITER.len()..].iter().collect());
                    break;
                }
            }

            // TextStringBuilder#midString clamps to the current size (escapes may have shrunk it).
            let mid_end = (offset + length).min(builder.len());
            let list =
                priors.get_or_insert_with(|| vec![builder[offset..mid_end].iter().collect()]);
            check_cyclic_substitution(&var_name, list)?;
            list.push(var_name.clone());

            if let Some(value) = vars.get(&var_name).cloned().or(var_default) {
                let value: Vec<char> = value.chars().collect();
                let var_len = value.len();
                builder.splice(start_pos..end_pos, value);
                altered = true;
                let nested = substitute(builder, start_pos, var_len, priors, vars)?.1;
                let change = nested + var_len as isize - (end_pos - start_pos) as isize;
                pos = pos.wrapping_add_signed(change);
                buf_end = buf_end.wrapping_add_signed(change);
                length_change += change;
            }

            if let Some(list) = priors.as_mut() {
                list.pop();
            }
            break;
        }
    }

    Ok((altered, length_change))
}

fn check_cyclic_substitution(var_name: &str, priors: &[String]) -> Result<(), SubstitutionError> {
    if !priors.iter().any(|p| p == var_name) {
        return Ok(());
    }
    Err(SubstitutionError::InfiniteLoop(format!(
        "Infinite loop in property interpolation of {}: {}",
        priors[0],
        priors[1..].join("->")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn r(s: &str, vars: &HashMap<String, String>) -> String {
        replace(s, vars).unwrap()
    }

    #[test]
    fn simple_and_missing() {
        let e = env(&[("DB", "dhis"), ("EMPTY", "")]);
        assert_eq!(
            r("jdbc:postgresql://${DB}/x", &e),
            "jdbc:postgresql://dhis/x"
        );
        assert_eq!(r("${MISSING}", &e), "${MISSING}");
        assert_eq!(r("[${EMPTY}]", &e), "[]");
        assert_eq!(r("no vars", &e), "no vars");
    }

    #[test]
    fn default_values() {
        let e = env(&[("SET", "v")]);
        assert_eq!(r("${UNSET:-fallback}", &e), "fallback");
        assert_eq!(r("${SET:-fallback}", &e), "v");
        assert_eq!(r("${UNSET:-}", &e), "");
        assert_eq!(r("${UNSET:-a:-b}", &e), "a:-b");
    }

    #[test]
    fn escapes() {
        let e = env(&[("X", "v")]);
        assert_eq!(r("$${X}", &e), "${X}");
        assert_eq!(r("$${X} ${X}", &e), "${X} v");
        assert_eq!(r("a$b", &e), "a$b");
    }

    #[test]
    fn unterminated() {
        let e = env(&[("X", "v")]);
        assert_eq!(r("${X", &e), "${X");
        assert_eq!(r("${X} ${X", &e), "v ${X");
    }

    #[test]
    fn recursive_values() {
        let e = env(&[("A", "${B}-a"), ("B", "b")]);
        assert_eq!(r("${A}!", &e), "b-a!");
    }

    #[test]
    fn cycles_fail_like_java() {
        let e = env(&[("A", "${B}"), ("B", "${A}")]);
        assert_eq!(
            replace("x${A}", &e),
            Err(SubstitutionError::InfiniteLoop(
                "Infinite loop in property interpolation of x${A}: A->B".into()
            ))
        );
    }

    #[test]
    fn same_variable_twice_is_not_a_cycle() {
        let e = env(&[("A", "a")]);
        assert_eq!(r("${A}${A}", &e), "aa");
    }
}
