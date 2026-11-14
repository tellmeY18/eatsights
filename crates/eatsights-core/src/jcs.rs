//! RFC 8785 JSON Canonicalization Scheme (JCS).

use serde_json::Value;

#[derive(Debug, thiserror::Error)]
#[error("canonicalisation failed: {0}")]
pub struct JcsError(String);

/// Canonical JSON text of `value` (RFC 8785).
pub fn to_string(value: &Value) -> Result<String, JcsError> {
    serde_json_canonicalizer::to_string(value).map_err(|e| JcsError(e.to_string()))
}

/// Canonical JSON bytes (UTF-8) of `value` (RFC 8785).
pub fn to_vec(value: &Value) -> Result<Vec<u8>, JcsError> {
    serde_json_canonicalizer::to_vec(value).map_err(|e| JcsError(e.to_string()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    // RFC 8785 section 3.2.2.
    #[test]
    fn rfc8785_main_example() {
        let input = r#"{
          "numbers": [333333333.33333329, 1E30, 4.50, 2e-3, 0.000000000000000000000000001],
          "string": "\u20ac$\u000F\u000aA'\u0042\u0022\u005c\\\"\/",
          "literals": [null, true, false]
        }"#;
        let v: Value = serde_json::from_str(input).unwrap();
        assert_eq!(
            to_string(&v).unwrap(),
            r#"{"literals":[null,true,false],"numbers":[333333333.3333333,1e+30,4.5,0.002,1e-27],"string":"€$\u000f\nA'B\"\\\\\"/"}"#
        );
    }

    // RFC 8785 section 3.2.3: keys sorted by UTF-16 code units.
    #[test]
    fn rfc8785_sorting() {
        let input = r#"{
          "\u20ac": "Euro Sign",
          "\r": "Carriage Return",
          "\ufb33": "Hebrew Letter Dalet With Dagesh",
          "1": "One",
          "\ud83d\ude00": "Emoji: Grinning Face",
          "\u0080": "Control",
          "\u00f6": "Latin Small Letter O With Diaeresis"
        }"#;
        let v: Value = serde_json::from_str(input).unwrap();
        let expected = "{\"\\r\":\"Carriage Return\",\"1\":\"One\",\"\u{80}\":\"Control\",\"\u{f6}\":\"Latin Small Letter O With Diaeresis\",\"\u{20ac}\":\"Euro Sign\",\"\u{1f600}\":\"Emoji: Grinning Face\",\"\u{fb33}\":\"Hebrew Letter Dalet With Dagesh\"}";
        assert_eq!(to_string(&v).unwrap(), expected);
    }

    // RFC 8785 appendix B number samples.
    #[test]
    fn rfc8785_numbers() {
        let cases = [
            (0.0f64, "0"),
            (-0.0, "0"),
            (5e-324, "5e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            (9007199254740992.0, "9007199254740992"),
            (295147905179352830000.0, "295147905179352830000"),
            (1e21, "1e+21"),
            (1e-7, "1e-7"),
            (0.000001, "0.000001"),
            (333333333.3333332, "333333333.3333332"),
            (-5e-324, "-5e-324"),
        ];
        for (n, s) in cases {
            assert_eq!(to_string(&serde_json::json!(n)).unwrap(), s, "{n}");
        }
    }
}
