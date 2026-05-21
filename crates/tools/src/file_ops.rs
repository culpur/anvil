use runtime::{edit_file, read_file, write_file};
use serde::{Deserialize, Deserializer};

use crate::{io_to_string, to_pretty_json};

// ───────────────────────────────────────────────────────────────────────────
// CC parity (v2.1.144-B7, task #723): file mime-type mismatch fallback.
//
// `runtime::read_file` only handles UTF-8 text; it surfaces an io error on a
// non-UTF-8 file. The Read tool aborts when a file's binary signature
// contradicts its extension — e.g. a `.txt` whose bytes are actually a PNG.
//
// Fallback order in `run_read_file`:
//   1. Try the extension-based handler (text for non-image extensions,
//      image for known image extensions).
//   2. On parse/read error, sniff the first 16 bytes for an image magic
//      number (PNG / JPEG / GIF / WebP).
//   3. If an image signature is detected and step 1 was text, emit an image
//      attachment payload instead of failing.
//   4. If the file is a "looks-text" UTF-8 file but the extension says
//      image, fall back to text read.
//   5. If both paths fail, surface the original error with a mismatch note.
// ───────────────────────────────────────────────────────────────────────────

/// Image magic-number sniff result for the first ≤ 16 bytes of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SniffedKind {
    Png,
    Jpeg,
    Gif,
    Webp,
    /// No recognised image signature.
    Unknown,
}

impl SniffedKind {
    fn mime(self) -> Option<&'static str> {
        match self {
            Self::Png => Some("image/png"),
            Self::Jpeg => Some("image/jpeg"),
            Self::Gif => Some("image/gif"),
            Self::Webp => Some("image/webp"),
            Self::Unknown => None,
        }
    }
}

/// Sniff the first ≤ 16 bytes of `data` for a known image signature.
///
/// Magic numbers:
///   PNG : 89 50 4E 47 0D 0A 1A 0A
///   JPEG: FF D8 FF
///   GIF : "GIF87a" / "GIF89a"
///   WebP: "RIFF" .... "WEBP"
fn sniff_image_magic(data: &[u8]) -> SniffedKind {
    if data.len() >= 8
        && data[0] == 0x89
        && &data[1..4] == b"PNG"
        && data[4] == 0x0D
        && data[5] == 0x0A
        && data[6] == 0x1A
        && data[7] == 0x0A
    {
        return SniffedKind::Png;
    }
    if data.len() >= 3 && data[0] == 0xFF && data[1] == 0xD8 && data[2] == 0xFF {
        return SniffedKind::Jpeg;
    }
    if data.len() >= 6 && (&data[..6] == b"GIF87a" || &data[..6] == b"GIF89a") {
        return SniffedKind::Gif;
    }
    if data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        return SniffedKind::Webp;
    }
    SniffedKind::Unknown
}

/// True if the extension is one of the image types we treat as a binary
/// read. Anything else (including no extension) is treated as text.
fn extension_is_image(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
        .is_some_and(|ext| matches!(ext, "png" | "jpg" | "jpeg" | "gif" | "webp"))
}

/// Build the image-attachment JSON envelope returned when we successfully
/// fell back from a failing text read.
fn image_fallback_payload(
    absolute_path: &std::path::Path,
    sniffed: SniffedKind,
    bytes: &[u8],
) -> String {
    use base64::Engine;
    let mime = sniffed.mime().unwrap_or("application/octet-stream");
    let data_b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    let value = serde_json::json!({
        "type": "image",
        "file": {
            "filePath": absolute_path.display().to_string(),
            "mediaType": mime,
            "data": data_b64,
            "bytes": bytes.len(),
            "mismatchNote": "binary image signature detected; extension expected text",
        },
    });
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
}

/// Build the text-attachment JSON envelope returned when we successfully
/// fell back from a failing image read (image extension, UTF-8 content).
fn text_fallback_payload(absolute_path: &std::path::Path, content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let value = serde_json::json!({
        "type": "text",
        "file": {
            "filePath": absolute_path.display().to_string(),
            "content": content,
            "numLines": lines.len(),
            "startLine": 1,
            "totalLines": lines.len(),
            "mismatchNote": "UTF-8 text detected; extension expected image",
        },
    });
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
}

/// Read up to `limit` bytes from `path` as raw bytes. Returns Err if the
/// file is missing or unreadable.
fn read_raw_bytes(path: &str, limit: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut buf = Vec::with_capacity(limit.min(64 * 1024));
    f.take(limit as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

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
    // CC parity (v2.1.144-B7, task #723): mime-type mismatch fallback.
    //
    // Step 1 — pick the primary handler from the extension.
    let is_image_ext = extension_is_image(&input.path);

    if is_image_ext {
        // Image extension first; on parse-as-image failure, fall back to
        // text read in case the file is actually UTF-8 (e.g. someone
        // renamed a README to README.png).
        match read_raw_bytes(&input.path, 8 * 1024 * 1024) {
            Ok(bytes) => {
                let sniffed = sniff_image_magic(&bytes);
                if sniffed != SniffedKind::Unknown {
                    // Real image — return image payload.
                    let absolute_path = std::fs::canonicalize(&input.path)
                        .unwrap_or_else(|_| std::path::PathBuf::from(&input.path));
                    Ok(image_fallback_payload(&absolute_path, sniffed, &bytes))
                } else {
                    // Not actually an image — try text path.
                    match read_file(&input.path, input.offset, input.limit) {
                        Ok(_) => {
                            // Successful text fallback: return text-fallback
                            // envelope with mismatch note.
                            let absolute_path = std::fs::canonicalize(&input.path)
                                .unwrap_or_else(|_| std::path::PathBuf::from(&input.path));
                            let content = std::fs::read_to_string(&input.path)
                                .unwrap_or_default();
                            Ok(text_fallback_payload(&absolute_path, &content))
                        }
                        Err(_) => Err(format!(
                            "file {} has image extension but neither a recognised image \
                             signature nor valid UTF-8 text",
                            input.path
                        )),
                    }
                }
            }
            Err(e) => Err(io_to_string(e)),
        }
    } else {
        // Text extension (or none). Try text first.
        match read_file(&input.path, input.offset, input.limit) {
            Ok(out) => to_pretty_json(out),
            Err(primary_err) => {
                // Sniff the file head for an image signature.
                if let Ok(bytes) = read_raw_bytes(&input.path, 8 * 1024 * 1024) {
                    let sniffed = sniff_image_magic(&bytes);
                    if sniffed != SniffedKind::Unknown {
                        let absolute_path = std::fs::canonicalize(&input.path)
                            .unwrap_or_else(|_| std::path::PathBuf::from(&input.path));
                        return Ok(image_fallback_payload(&absolute_path, sniffed, &bytes));
                    }
                }
                Err(format!(
                    "read failed and no image signature detected: {}",
                    io_to_string(primary_err)
                ))
            }
        }
    }
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

    // ───────────────────────────────────────────────────────────────────
    // CC parity (v2.1.144-B7, task #723): mime-type mismatch fallback.
    // Three regression cases:
    //   1. `.txt` file with PNG bytes → image-fallback payload.
    //   2. `.png` file with UTF-8 text → text-fallback payload.
    //   3. `.txt` file with valid UTF-8 → normal text path.
    // ───────────────────────────────────────────────────────────────────

    use super::{run_read_file, sniff_image_magic, SniffedKind};
    use std::io::Write;

    fn write_tmp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "anvil-test-mime-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).expect("create tmp file");
        f.write_all(bytes).expect("write tmp file");
        path
    }

    /// PNG magic = 89 50 4E 47 0D 0A 1A 0A then non-UTF-8 trailing bytes.
    const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\nrest-of-binary-blob-not-utf8\xff\xfe";

    #[test]
    fn txt_with_png_content_falls_back_to_image_path() {
        let path = write_tmp("looks-like.txt", PNG_MAGIC);
        let json = run_read_file(super::ReadFileInput {
            path: path.display().to_string(),
            offset: None,
            limit: None,
        })
        .expect("expected image fallback to succeed");
        assert!(
            json.contains("\"type\": \"image\"") && json.contains("image/png"),
            "expected image-fallback envelope, got: {json}"
        );
    }

    #[test]
    fn png_named_file_with_utf8_text_falls_back_to_text_path() {
        let text = "Hello — this file is named .png but is actually UTF-8 text.\nLine 2.\n";
        let path = write_tmp("misnamed.png", text.as_bytes());
        let json = run_read_file(super::ReadFileInput {
            path: path.display().to_string(),
            offset: None,
            limit: None,
        })
        .expect("expected text fallback to succeed");
        assert!(
            json.contains("\"type\": \"text\"") && json.contains("Hello"),
            "expected text-fallback envelope, got: {json}"
        );
    }

    #[test]
    fn txt_with_utf8_content_uses_normal_text_path() {
        let path = write_tmp("normal.txt", b"plain ascii line 1\nplain ascii line 2\n");
        let json = run_read_file(super::ReadFileInput {
            path: path.display().to_string(),
            offset: None,
            limit: None,
        })
        .expect("expected normal text read to succeed");
        // Normal path: `runtime::read_file` returns `kind: "text"` already,
        // without a mismatchNote.
        assert!(json.contains("\"type\": \"text\""), "got: {json}");
        assert!(!json.contains("mismatchNote"), "normal path must not carry mismatch note");
        assert!(json.contains("plain ascii line 1"), "got: {json}");
    }

    #[test]
    fn sniff_recognises_png_jpeg_gif_webp() {
        assert_eq!(sniff_image_magic(b"\x89PNG\r\n\x1a\nXXX"), SniffedKind::Png);
        assert_eq!(sniff_image_magic(b"\xff\xd8\xff\xe0JFIF"), SniffedKind::Jpeg);
        assert_eq!(sniff_image_magic(b"GIF89a___"), SniffedKind::Gif);
        let mut webp = Vec::from(b"RIFF" as &[u8]);
        webp.extend_from_slice(&[0, 0, 0, 0]);
        webp.extend_from_slice(b"WEBPanything");
        assert_eq!(sniff_image_magic(&webp), SniffedKind::Webp);
        assert_eq!(sniff_image_magic(b"plain text"), SniffedKind::Unknown);
    }
}
