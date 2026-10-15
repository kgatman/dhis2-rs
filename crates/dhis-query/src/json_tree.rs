//! Builds a [`Node`] tree from JSON that Jackson wrote, keeping member order.
//!
//! Used for data captured from Java (e.g. the registered schemas, ADR 0007): objects whose
//! property has a known schema become [`Node::Bean`]s of that class, so the field filter treats
//! them as Java does; anything else stays a map or scalar.

use serde::Deserialize;
use serde::de::{Deserializer, MapAccess, SeqAccess, Visitor};

use crate::field_filter::Node;

/// JSON with object members in document order.
#[derive(Debug, Clone, PartialEq)]
enum Ordered {
    Scalar(serde_json::Value),
    Array(Vec<Ordered>),
    Object(Vec<(String, Ordered)>),
}

impl<'de> Deserialize<'de> for Ordered {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Ordered;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("JSON")
            }
            fn visit_unit<E>(self) -> Result<Ordered, E> {
                Ok(Ordered::Scalar(serde_json::Value::Null))
            }
            fn visit_bool<E>(self, v: bool) -> Result<Ordered, E> {
                Ok(Ordered::Scalar(v.into()))
            }
            fn visit_i64<E>(self, v: i64) -> Result<Ordered, E> {
                Ok(Ordered::Scalar(v.into()))
            }
            fn visit_u64<E>(self, v: u64) -> Result<Ordered, E> {
                Ok(Ordered::Scalar(v.into()))
            }
            fn visit_f64<E>(self, v: f64) -> Result<Ordered, E> {
                Ok(Ordered::Scalar(v.into()))
            }
            fn visit_str<E>(self, v: &str) -> Result<Ordered, E> {
                Ok(Ordered::Scalar(v.into()))
            }
            fn visit_string<E>(self, v: String) -> Result<Ordered, E> {
                Ok(Ordered::Scalar(v.into()))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Ordered, A::Error> {
                let mut out = Vec::new();
                while let Some(v) = seq.next_element()? {
                    out.push(v);
                }
                Ok(Ordered::Array(out))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Ordered, A::Error> {
                let mut out = Vec::new();
                while let Some((k, v)) = map.next_entry()? {
                    out.push((k, v));
                }
                Ok(Ordered::Object(out))
            }
        }
        d.deserialize_any(V)
    }
}

/// Parses `json` as an instance of `klass` (or an array of them).
pub fn bean_from_json(json: &str, klass: &'static str) -> Result<Node, String> {
    let v: Ordered = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(convert(v, Some(klass)))
}

fn convert(v: Ordered, klass: Option<&'static str>) -> Node {
    match v {
        Ordered::Scalar(s) => Node::Scalar(s),
        Ordered::Array(items) => {
            Node::Array(items.into_iter().map(|i| convert(i, klass)).collect())
        }
        Ordered::Object(members) => {
            let schema = klass.and_then(dhis_schema::schema);
            let props = members
                .into_iter()
                .map(|(k, v)| {
                    // The class of this member's value (item class for collections), if it has a
                    // schema of its own.
                    let child = schema
                        .and_then(|s| s.properties.values().find(|p| p.output_name() == k))
                        .and_then(|p| p.value_klass())
                        .and_then(dhis_schema::schema)
                        .map(|s| s.klass.as_str());
                    let node = convert(v, child);
                    (k, node)
                })
                .collect();
            match schema {
                Some(s) => Node::Bean {
                    klass: s.klass.as_str(),
                    props,
                },
                None => Node::Map(props),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_beans_by_schema() {
        let n = bean_from_json(
            r#"{"singular":"x","authorities":[{"type":"DELETE","authorities":["F_X"]}],
                "properties":[{"name":"p","gistPreferences":{"included":"AUTO"}}]}"#,
            "org.hisp.dhis.schema.Schema",
        )
        .unwrap();
        let Node::Bean { klass, props } = &n else {
            panic!("{n:?}")
        };
        assert_eq!(*klass, "org.hisp.dhis.schema.Schema");
        assert_eq!(props[0].0, "singular");
        let Node::Array(auths) = &props[1].1 else {
            panic!()
        };
        assert!(
            matches!(&auths[0], Node::Bean { klass, .. } if *klass == "org.hisp.dhis.security.Authority")
        );
        let Node::Array(ps) = &props[2].1 else {
            panic!()
        };
        let Node::Bean { props: pp, .. } = &ps[0] else {
            panic!()
        };
        assert!(
            matches!(&pp[1].1, Node::Bean { klass, .. } if *klass == "org.hisp.dhis.schema.GistPreferences")
        );
    }
}
