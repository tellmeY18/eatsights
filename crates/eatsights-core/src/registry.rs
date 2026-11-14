//! The property registry (`properties.json`), typed and with compiled value validators.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use jsonschema::Validator;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::schema;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Place,
    MenuItem,
    Dish,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Place => "place",
            Scope::MenuItem => "menu_item",
            Scope::Dish => "dish",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "place" => Some(Scope::Place),
            "menu_item" => Some(Scope::MenuItem),
            "dish" => Some(Scope::Dish),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Writer {
    Customer,
    Vendor,
    Import,
    System,
}

impl Writer {
    pub fn as_str(self) -> &'static str {
        match self {
            Writer::Customer => "customer",
            Writer::Vendor => "vendor",
            Writer::Import => "import",
            Writer::System => "system",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    LatestWeighted,
    Median,
    Mode,
    Union,
    Schedule,
    Derived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PropertyStatus {
    Proposed,
    Accepted,
    Deprecated,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Aggregation {
    pub strategy: Strategy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Property {
    pub id: String,
    pub scope: Scope,
    pub label: BTreeMap<String, String>,
    pub description: String,
    pub value_schema: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    pub half_life_hours: f64,
    pub writers: Vec<Writer>,
    pub aggregation: Aggregation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_from: Option<Vec<String>>,
    pub status: PropertyStatus,
    pub since: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_by: Option<String>,
}

impl Property {
    /// True when clients may write it (anything other than `["system"]`-only).
    pub fn client_writable(&self) -> bool {
        self.writers.iter().any(|w| *w != Writer::System)
    }

    pub fn allows_writer(&self, w: Writer) -> bool {
        self.writers.contains(&w)
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum RegistryError {
    #[error("properties.json is not valid JSON: {0}")]
    Json(String),
    #[error("registry entry {index} ({id}) is invalid: {errors:?}")]
    Entry {
        index: usize,
        id: String,
        errors: Vec<String>,
    },
    #[error("duplicate property id {0}")]
    Duplicate(String),
    #[error("value_schema of {id} does not compile: {error}")]
    ValueSchema { id: String, error: String },
}

struct Entry {
    property: Property,
    raw: Value,
    validator: Validator,
}

pub struct Registry {
    version: String,
    entries: BTreeMap<String, Entry>,
    hash: String,
}

#[derive(Deserialize)]
struct File {
    registry_version: String,
    properties: Vec<Value>,
}

impl Registry {
    /// Parse a registry file, validating each entry against `property.schema.json`
    /// and compiling each `value_schema`.
    pub fn from_json(text: &str) -> Result<Self, RegistryError> {
        let file: File =
            serde_json::from_str(text).map_err(|e| RegistryError::Json(e.to_string()))?;
        let mut entries = BTreeMap::new();
        for (index, raw) in file.properties.into_iter().enumerate() {
            let id = raw
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_owned();
            schema::validate_property_entry(&raw).map_err(|errors| RegistryError::Entry {
                index,
                id: id.clone(),
                errors,
            })?;
            let property: Property =
                serde_json::from_value(raw.clone()).map_err(|e| RegistryError::Entry {
                    index,
                    id: id.clone(),
                    errors: vec![e.to_string()],
                })?;
            let validator = schema::compile(&property.value_schema).map_err(|e| {
                RegistryError::ValueSchema {
                    id: id.clone(),
                    error: e.0,
                }
            })?;
            if entries.contains_key(&id) {
                return Err(RegistryError::Duplicate(id));
            }
            entries.insert(
                id,
                Entry {
                    property,
                    raw,
                    validator,
                },
            );
        }
        Ok(Self {
            version: file.registry_version,
            entries,
            hash: crate::crypto::sha256_hex(text.as_bytes()),
        })
    }

    /// The registry embedded at build time (parsed once).
    pub fn embedded() -> Result<&'static Registry, RegistryError> {
        static REG: OnceLock<Result<Registry, RegistryError>> = OnceLock::new();
        REG.get_or_init(|| Registry::from_json(schema::PROPERTIES_JSON))
            .as_ref()
            .map_err(Clone::clone)
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// Lower-case hex SHA-256 of the registry file this was built from.
    pub fn hash(&self) -> &str {
        &self.hash
    }

    pub fn get(&self, id: &str) -> Option<&Property> {
        self.entries.get(id).map(|e| &e.property)
    }

    /// The registry entry exactly as written in the file.
    pub fn get_raw(&self, id: &str) -> Option<&Value> {
        self.entries.get(id).map(|e| &e.raw)
    }

    /// Properties in id order.
    pub fn iter(&self) -> impl Iterator<Item = &Property> {
        self.entries.values().map(|e| &e.property)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Validate `value` against the `value_schema` of property `id`.
    pub fn validate_value(&self, id: &str, value: &Value) -> Result<(), Vec<String>> {
        match self.entries.get(id) {
            Some(e) => schema::errors(&e.validator, value),
            None => Err(vec![format!("unknown property `{id}`")]),
        }
    }
}

/// Lower-case hex SHA-256 of the embedded `properties.json` (for ETags).
pub fn registry_hash() -> String {
    crate::crypto::sha256_hex(schema::PROPERTIES_JSON.as_bytes())
}
