//! `java::hash::hash_set_order` against iteration orders printed by the server image's JDK
//! (`tools/gen-hashset-fixtures.sh`).

use dhis_core_model::java::hash::{Construction, hash_set_order};

#[test]
fn matches_jdk_fixtures() {
    let mut checked = 0;
    for line in include_str!("fixtures/hashset.tsv").lines() {
        let cols: Vec<&str> = line.split('\t').collect();
        let construction = match cols[0] {
            "presized" => Construction::Presized,
            "incremental" => Construction::Default,
            other => panic!("unknown kind {other}"),
        };
        let split = |s: &'static str| -> Vec<&'static str> {
            if s.is_empty() {
                vec![]
            } else {
                s.split('\u{1f}').collect()
            }
        };
        let input = split(cols[1]);
        let want = split(cols[2]);
        // Treeified bins are not modelled (None); the large random cases must still mostly pass.
        if let Some(got) = hash_set_order(input.iter().copied(), construction) {
            assert_eq!(got, want, "{} {:?}", cols[0], input);
            checked += 1;
        }
    }
    assert!(checked >= 150, "only {checked} fixtures checked");
}
