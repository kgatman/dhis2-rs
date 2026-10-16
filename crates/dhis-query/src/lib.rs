//! The `fields=`/`filter=`/`order=` DSL and the metadata query engine.
//!
//! Java counterparts: `org.hisp.dhis.common.PropertyPath`, `org.hisp.dhis.common.input.Fields`
//! (dhis-api), `org.hisp.dhis.fieldfiltering` (dhis-service-field-filtering).

pub mod field_filter;
pub mod fields;
pub mod gist;
pub mod json_tree;
pub mod metadata;
pub mod property_path;

/// Java's `IllegalArgumentException` with its exact message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct IllegalArgument(pub String);
