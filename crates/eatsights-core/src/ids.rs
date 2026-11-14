//! Prefixed 26-character Crockford ULID identifiers, as defined in `schema.json#/$defs/ids`.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid {kind} id: expected `{prefix}` followed by 26 Crockford base32 characters")]
pub struct IdError {
    pub kind: &'static str,
    pub prefix: &'static str,
}

fn is_crockford(c: u8) -> bool {
    matches!(c, b'0'..=b'9' | b'A'..=b'H' | b'J' | b'K' | b'M' | b'N' | b'P'..=b'T' | b'V'..=b'Z')
}

/// True when `s` is `prefix` followed by exactly 26 upper-case Crockford base32 characters.
pub fn is_valid_prefixed(prefix: &str, s: &str) -> bool {
    match s.strip_prefix(prefix) {
        Some(rest) => rest.len() == 26 && rest.bytes().all(is_crockford),
        None => false,
    }
}

/// True when `s` matches the schema's `key_id` pattern (`^[A-Za-z0-9_-]{22,64}$`).
pub fn is_valid_key_id(s: &str) -> bool {
    (22..=64).contains(&s.len())
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

macro_rules! prefixed_id {
    ($name:ident, $prefix:literal, $kind:literal) => {
        #[doc = concat!("`", $prefix, "` + ULID (", $kind, ").")]
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            pub const PREFIX: &'static str = $prefix;

            /// A fresh id from the current time and a random component.
            #[allow(clippy::new_without_default)]
            pub fn new() -> Self {
                Self(format!("{}{}", $prefix, ulid::Ulid::new()))
            }

            pub fn parse(s: &str) -> Result<Self, IdError> {
                if is_valid_prefixed($prefix, s) {
                    Ok(Self(s.to_owned()))
                } else {
                    Err(IdError {
                        kind: $kind,
                        prefix: $prefix,
                    })
                }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = IdError;
            fn from_str(s: &str) -> Result<Self, IdError> {
                Self::parse(s)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                Self::parse(&s).map_err(serde::de::Error::custom)
            }
        }
    };
}

prefixed_id!(PlaceId, "pl_", "place");
prefixed_id!(SectionId, "sc_", "section");
prefixed_id!(MenuItemId, "mi_", "menu item");
prefixed_id!(DishId, "di_", "dish");
prefixed_id!(ObservationId, "ob_", "observation");
prefixed_id!(AttestationId, "at_", "attestation");
prefixed_id!(EntitySubmissionId, "es_", "entity submission");

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn generate_and_parse() {
        let id = PlaceId::new();
        assert!(id.as_str().starts_with("pl_"));
        assert_eq!(id.as_str().len(), 29);
        assert_eq!(PlaceId::parse(id.as_str()).unwrap(), id);
        assert!(EntitySubmissionId::new().as_str().starts_with("es_"));
    }

    #[test]
    fn rejects_bad() {
        assert!(PlaceId::parse("pl_01JC8Y0A1B2C3D4E5F6G7H8J9K").is_ok());
        assert!(PlaceId::parse("sc_01JC8Y0A1B2C3D4E5F6G7H8J9K").is_err());
        assert!(PlaceId::parse("pl_01JC8Y0A1B2C3D4E5F6G7H8J9").is_err());
        assert!(PlaceId::parse("pl_01JC8Y0A1B2C3D4E5F6G7H8J9I").is_err()); // I excluded
        assert!(PlaceId::parse("pl_01jc8y0a1b2c3d4e5f6g7h8j9k").is_err()); // lower case
        assert!(PlaceId::parse("pl_01JC8Y0A1B2C3D4E5F6G7H8J9KX").is_err());
    }

    #[test]
    fn serde_roundtrip() {
        let id: DishId = serde_json::from_str("\"di_01JC8Y0A1B2C3D4E5F6G7H8J9K\"").unwrap();
        assert_eq!(
            serde_json::to_string(&id).unwrap(),
            "\"di_01JC8Y0A1B2C3D4E5F6G7H8J9K\""
        );
        assert!(serde_json::from_str::<DishId>("\"di_bad\"").is_err());
    }

    #[test]
    fn key_id_pattern() {
        assert!(is_valid_key_id("k7Qm2xWv9LpR4tYb8NcD3a"));
        assert!(!is_valid_key_id("short"));
        assert!(!is_valid_key_id("k7Qm2xWv9LpR4tYb8NcD3a+"));
    }
}
