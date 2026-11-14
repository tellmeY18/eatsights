# eatsights-core dependencies

| Crate | Why |
|---|---|
| `serde` | Derive (de)serialisation for ids and registry types. |
| `serde_json` | `Value` is the working form of every contract document. |
| `serde_json_canonicalizer` | RFC 8785 (JCS) output incl. ES6 number formatting and UTF-16 key order; small, no deps beyond serde/ryu. |
| `thiserror` | Typed error enums without boilerplate (no `unwrap` in non-test code). |
| `ulid` | Generates the 26-char Crockford ULIDs used in every id. |
| `chrono` (no default features) | RFC 3339 parsing and time-window checks on submissions. |
| `jsonschema` (no default features: no HTTP/file resolving) | Draft 2020-12 validator; needed for `unevaluatedProperties`, `if/then/else` and every `value_schema`. |
| `ed25519-dalek` (`rand_core`) | Ed25519 sign/verify (contract-pinned). |
| `rand_core` (`getrandom`) | OS randomness for `generate_signing_key` (keygen, tests, e2e). |
| `sha2` | SHA-256 for `key_id` and registry hash. |
| `hmac` | HMAC-SHA256 pseudonyms. |
| `base64` | base64url-nopad for keys, signatures, key ids and pseudonyms. |
