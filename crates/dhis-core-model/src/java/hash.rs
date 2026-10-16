//! `java.util.HashSet<String>` iteration order.
//!
//! Several DHIS2 responses serialise a `HashSet` (authorities, setting keys), so their order is
//! the hash table's: by bucket `(h ^ h >>> 16) & (capacity - 1)` of `String.hashCode()`, and by
//! insertion order within a bucket (resizing keeps that relative order). The final capacity
//! depends on how the set was built, and `HashSet(Collection)` sizing differs between JDK
//! releases; this follows **JDK 21**, the runtime of the server image (verified against fixtures
//! produced with it, `tools/gen-hashset-fixtures.sh`).
//!
//! A bucket that grows past 7 entries is turned into a tree (or forces a resize on small tables),
//! which changes the order; that is not modelled and yields `None`.

/// `String.hashCode()`: over UTF-16 code units, wrapping.
pub fn string_hash_code(s: &str) -> i32 {
    s.encode_utf16()
        .fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(i32::from(c)))
}

/// `HashMap.hash(key)`.
fn spread(s: &str) -> u32 {
    let h = string_hash_code(s) as u32;
    h ^ (h >> 16)
}

/// How the set was constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Construction {
    /// `new HashSet<>(collection)`: JDK 21 sizes it `HashMap.newHashMap(max(size, 12))`.
    Presized,
    /// `new HashSet<>()` (capacity 16) followed by `add`/`addAll`.
    Default,
}

/// `TREEIFY_THRESHOLD - 1`: a put that finds this many nodes already in its bin treeifies.
const TREEIFY_AT: usize = 7;

/// The iteration order of a `HashSet` built from `items` (in insertion order; duplicates keep
/// their first position, as in Java). `None` when a bin would be treeified.
pub fn hash_set_order<'a, I>(items: I, construction: Construction) -> Option<Vec<&'a str>>
where
    I: IntoIterator<Item = &'a str>,
{
    let items: Vec<&str> = items.into_iter().collect();
    let mut capacity: usize = match construction {
        // newHashMap(n) = new HashMap((int) Math.ceil(n / 0.75)); the table is tableSizeFor(that).
        Construction::Presized => {
            let n = items.len().max(12) as f64;
            ((n / 0.75).ceil() as usize).next_power_of_two()
        }
        Construction::Default => 16,
    };
    let mut set: Vec<(&str, u32)> = Vec::with_capacity(items.len());
    for item in items {
        if set.iter().any(|(s, _)| *s == item) {
            continue;
        }
        let h = spread(item);
        let mask = (capacity - 1) as u32;
        let in_bin = set.iter().filter(|(_, o)| o & mask == h & mask).count();
        if in_bin >= TREEIFY_AT {
            return None;
        }
        set.push((item, h));
        // ++size > threshold (0.75 * capacity) doubles the table.
        if set.len() > capacity * 3 / 4 {
            capacity *= 2;
        }
    }
    let mask = (capacity - 1) as u32;
    // Stable: equal buckets keep insertion order.
    set.sort_by_key(|(_, h)| h & mask);
    Some(set.into_iter().map(|(s, _)| s).collect())
}

/// The iteration order of a `HashSet`/`HashMap` whose distinct elements, in insertion order,
/// have these `hashCode()`s: indices into `hashes`. `None` when a bin would be treeified.
pub fn hash_order(hashes: &[i32], construction: Construction) -> Option<Vec<usize>> {
    let mut capacity: usize = match construction {
        Construction::Presized => {
            let n = hashes.len().max(12) as f64;
            ((n / 0.75).ceil() as usize).next_power_of_two()
        }
        Construction::Default => 16,
    };
    let mut table: Vec<(usize, u32)> = Vec::with_capacity(hashes.len());
    for (i, h) in hashes.iter().enumerate() {
        let h = *h as u32;
        let h = h ^ (h >> 16);
        let mask = (capacity - 1) as u32;
        if table.iter().filter(|(_, o)| o & mask == h & mask).count() >= TREEIFY_AT {
            return None;
        }
        table.push((i, h));
        if table.len() > capacity * 3 / 4 {
            capacity *= 2;
        }
    }
    let mask = (capacity - 1) as u32;
    table.sort_by_key(|(_, h)| h & mask);
    Some(table.into_iter().map(|(i, _)| i).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_code_matches_java() {
        assert_eq!(string_hash_code(""), 0);
        assert_eq!(string_hash_code("Aa"), 2112);
        assert_eq!(string_hash_code("BB"), 2112);
        assert_eq!(string_hash_code("ALL"), 64897);
        // jshell: "keyUiLocale".hashCode(), "😀".hashCode()
        assert_eq!(string_hash_code("keyUiLocale"), 1_763_048_845);
        assert_eq!(string_hash_code("\u{1F600}"), 1_772_899);
    }
}
