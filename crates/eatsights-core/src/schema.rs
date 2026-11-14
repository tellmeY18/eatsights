//! The embedded contract files and validators for `schema.json#/$defs/*`.

use std::collections::HashMap;
use std::sync::OnceLock;

use jsonschema::{Resource, Validator};
use serde_json::{Value, json};

/// `schema.json`, verbatim.
pub const SCHEMA_JSON: &str = include_str!("../../../schema.json");
/// `property.schema.json`, verbatim.
pub const PROPERTY_SCHEMA_JSON: &str = include_str!("../../../property.schema.json");
/// `properties.json`, verbatim.
pub const PROPERTIES_JSON: &str = include_str!("../../../properties.json");

/// Top-level `$defs` that can be validated with [`validate_def`].
pub const DEFS: &[&str] = &[
    "place",
    "section",
    "dish",
    "menu_item",
    "subject",
    "submission",
    "observation",
    "key",
    "attestation",
    "fact",
    "osm_link",
    "lang_map",
    "link",
];

#[derive(Debug, Clone, thiserror::Error)]
#[error("schema error: {0}")]
pub struct SchemaError(pub String);

struct Validators {
    defs: HashMap<&'static str, Validator>,
    property: Validator,
}

static VALIDATORS: OnceLock<Result<Validators, SchemaError>> = OnceLock::new();

fn schema_id(schema: &Value) -> Result<String, SchemaError> {
    schema
        .get("$id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| SchemaError("schema.json has no $id".into()))
}

fn build() -> Result<Validators, SchemaError> {
    let schema: Value =
        serde_json::from_str(SCHEMA_JSON).map_err(|e| SchemaError(format!("schema.json: {e}")))?;
    let id = schema_id(&schema)?;
    let mut defs = HashMap::new();
    for def in DEFS {
        let wrapper = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$ref": format!("{id}#/$defs/{def}"),
        });
        let resource = Resource::from_contents(schema.clone())
            .map_err(|e| SchemaError(format!("schema.json resource: {e}")))?;
        let v = jsonschema::draft202012::options()
            .should_validate_formats(true)
            .with_resource(id.as_str(), resource)
            .build(&wrapper)
            .map_err(|e| SchemaError(format!("$defs/{def}: {e}")))?;
        defs.insert(*def, v);
    }
    let prop_schema: Value = serde_json::from_str(PROPERTY_SCHEMA_JSON)
        .map_err(|e| SchemaError(format!("property.schema.json: {e}")))?;
    let property = compile(&prop_schema)?;
    Ok(Validators { defs, property })
}

fn validators() -> Result<&'static Validators, SchemaError> {
    VALIDATORS.get_or_init(build).as_ref().map_err(Clone::clone)
}

/// Force compilation of every validator (call at startup to fail fast).
pub fn init() -> Result<(), SchemaError> {
    validators().map(|_| ())
}

/// Compile a standalone JSON Schema (draft 2020-12, formats asserted).
pub fn compile(schema: &Value) -> Result<Validator, SchemaError> {
    jsonschema::draft202012::options()
        .should_validate_formats(true)
        .build(schema)
        .map_err(|e| SchemaError(e.to_string()))
}

/// Validate with a compiled validator, returning every error as `"<path>: <message>"`.
pub fn errors(validator: &Validator, value: &Value) -> Result<(), Vec<String>> {
    let errs: Vec<String> = validator
        .iter_errors(value)
        .map(|e| {
            let path = e.instance_path.to_string();
            if path.is_empty() {
                format!("/: {e}")
            } else {
                format!("{path}: {e}")
            }
        })
        .collect();
    if errs.is_empty() { Ok(()) } else { Err(errs) }
}

/// Validate `value` against `schema.json#/$defs/<def>`. `def` must be one of [`DEFS`].
pub fn validate_def(def: &str, value: &Value) -> Result<(), Vec<String>> {
    let v = validators().map_err(|e| vec![e.to_string()])?;
    match v.defs.get(def) {
        Some(validator) => errors(validator, value),
        None => Err(vec![format!("unknown schema definition `{def}`")]),
    }
}

/// Validate one registry entry against `property.schema.json`.
pub fn validate_property_entry(value: &Value) -> Result<(), Vec<String>> {
    let v = validators().map_err(|e| vec![e.to_string()])?;
    errors(&v.property, value)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn compiles() {
        init().unwrap();
    }

    #[test]
    fn unknown_def() {
        assert!(validate_def("nope", &json!({})).is_err());
    }

    #[test]
    fn nested_refs_resolve() {
        let ok = json!({ "id": "di_01JC8Y0A1B2C3D4E5F6G7H8J9K", "labels": { "en": "Tea" } });
        validate_def("dish", &ok).unwrap();
        let bad = json!({ "id": "pl_01JC8Y0A1B2C3D4E5F6G7H8J9K", "labels": { "en": "Tea" } });
        assert!(validate_def("dish", &bad).is_err());
    }

    #[test]
    fn formats_are_asserted() {
        let s = json!({ "id": "sc_01JC8Y0A1B2C3D4E5F6G7H8J9K", "place_id": "pl_01JC8Y0A1B2C3D4E5F6G7H8J9K", "labels": {"en": "x"} });
        validate_def("section", &s).unwrap();
        let k = json!({ "key_id": "k7Qm2xWv9LpR4tYb8NcD3a", "algorithm": "ed25519",
            "public_key": "A".repeat(43), "created_at": "yesterday",
            "status": "active", "level": "root", "depth": 0 });
        assert!(validate_def("key", &k).is_err());
    }
}
