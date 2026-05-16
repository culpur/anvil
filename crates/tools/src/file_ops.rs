use runtime::{edit_file, read_file, write_file};
use serde::{Deserialize, Deserializer};

use crate::{io_to_string, to_pretty_json};

/// Lenient deserializer for `Option<usize>` that accepts:
/// - JSON null → None
/// - JSON number → Some(n)
/// - JSON string → trim whitespace + strip leading `+` + parse → Some(n)
///
/// CC-140-B parity: models occasionally send `"  5"` or `"+5"` as a JSON
/// string rather than a number.  The default serde `usize` deserializer
/// rejects strings, causing an opaque parse error.  This deserializer
/// normalises both forms.
fn deserialize_optional_usize<'de, D>(deserializer: D) -> Result<Option<usize>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::{self, Visitor};
    use std::fmt;

    struct OptUsizeVisitor;

    impl<'de> Visitor<'de> for OptUsizeVisitor {
        type Value = Option<usize>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "a non-negative integer, a numeric string, or null")
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_some<D2: Deserializer<'de>>(self, d: D2) -> Result<Self::Value, D2::Error> {
            d.deserialize_any(OptUsizeVisitor)
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
            usize::try_from(v)
                .map(Some)
                .map_err(|_| de::Error::custom(format!("integer {v} overflows usize")))
        }

        fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
            if v < 0 {
                return Err(de::Error::custom(format!(
                    "offset must be non-negative, got {v}"
                )));
            }
            usize::try_from(v as u64)
                .map(Some)
                .map_err(|_| de::Error::custom(format!("integer {v} overflows usize")))
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            let trimmed = v.trim().trim_start_matches('+');
            trimmed.parse::<usize>().map(Some).map_err(|_| {
                de::Error::custom(format!(
                    "cannot parse offset from string {v:?}: expected a non-negative integer"
                ))
            })
        }

        fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
            self.visit_str(&v)
        }
    }

    deserializer.deserialize_option(OptUsizeVisitor)
}

#[derive(Debug, Deserialize)]
pub(crate) struct ReadFileInput {
    pub(crate) path: String,
    #[serde(default, deserialize_with = "deserialize_optional_usize")]
    pub(crate) offset: Option<usize>,
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WriteFileInput {
    pub(crate) path: String,
    pub(crate) content: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct EditFileInput {
    pub(crate) path: String,
    pub(crate) old_string: String,
    pub(crate) new_string: String,
    pub(crate) replace_all: Option<bool>,
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_read_file(input: ReadFileInput) -> Result<String, String> {
    to_pretty_json(read_file(&input.path, input.offset, input.limit).map_err(io_to_string)?)
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_write_file(input: WriteFileInput) -> Result<String, String> {
    to_pretty_json(write_file(&input.path, &input.content).map_err(io_to_string)?)
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_edit_file(input: EditFileInput) -> Result<String, String> {
    to_pretty_json(
        edit_file(
            &input.path,
            &input.old_string,
            &input.new_string,
            input.replace_all.unwrap_or(false),
        )
        .map_err(io_to_string)?,
    )
}

#[cfg(test)]
mod tests {
    use super::ReadFileInput;

    fn parse(json: &str) -> ReadFileInput {
        serde_json::from_str(json).expect("parse failed")
    }

    #[test]
    fn offset_parses_from_number() {
        let input = parse(r#"{"path":"/tmp/x","offset":5}"#);
        assert_eq!(input.offset, Some(5));
    }

    #[test]
    fn offset_parses_from_whitespace_padded_string() {
        let input = parse(r#"{"path":"/tmp/x","offset":"  5"}"#);
        assert_eq!(input.offset, Some(5));
    }

    #[test]
    fn offset_parses_from_plus_prefixed_string() {
        let input = parse(r#"{"path":"/tmp/x","offset":"+5"}"#);
        assert_eq!(input.offset, Some(5));
    }

    #[test]
    fn offset_parses_from_null_as_none() {
        let input = parse(r#"{"path":"/tmp/x","offset":null}"#);
        assert_eq!(input.offset, None);
    }

    #[test]
    fn offset_rejects_garbage_string_with_clear_error() {
        let err = serde_json::from_str::<ReadFileInput>(
            r#"{"path":"/tmp/x","offset":"abc"}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("cannot parse offset"),
            "expected descriptive error, got: {err}"
        );
    }
}
