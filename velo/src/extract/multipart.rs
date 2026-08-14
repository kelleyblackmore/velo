//! `multipart/form-data` request bodies.
//!
//! Parsing happens over the already-buffered body, so this is a scanner rather
//! than a streaming state machine — no partial-boundary bookkeeping, and the
//! whole thing is straight-line byte comparison. The size ceiling that applies
//! to every body applies here too, so an upload is bounded before a byte of it
//! is examined.

use crate::error::{ApiError, FieldError};
use crate::extract::de::Pairs;
use crate::extract::FromRequest;
use crate::operation::{OperationContext, OperationInput};
use crate::request::Request;
use bytes::Bytes;
use velo_openapi::{AdditionalProperties, MediaType, RequestBody, Schema};

/// The most parts a single body may contain.
///
/// The body size limit alone does not bound part *count*: a few megabytes of
/// empty parts is millions of allocations. This caps the work regardless.
pub const MAX_PARTS: usize = 256;

/// One part of a multipart body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    name: String,
    file_name: Option<String>,
    content_type: Option<String>,
    data: Bytes,
}

impl Part {
    /// The `name` from the part's `Content-Disposition`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The `filename`, when the part came from a file input.
    pub fn file_name(&self) -> Option<&str> {
        self.file_name.as_deref()
    }

    /// The part's own `Content-Type`, if it declared one.
    pub fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }

    /// The raw bytes.
    pub fn bytes(&self) -> &Bytes {
        &self.data
    }

    /// Takes the bytes, which is free — they share the request buffer.
    pub fn into_bytes(self) -> Bytes {
        self.data
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// The bytes as text, if they are valid UTF-8.
    pub fn text(&self) -> Option<&str> {
        std::str::from_utf8(&self.data).ok()
    }

    /// True when the part carried a filename, which is how a browser marks a
    /// file input as distinct from a text field.
    pub fn is_file(&self) -> bool {
        self.file_name.is_some()
    }
}

/// A parsed `multipart/form-data` body.
///
/// ```ignore
/// #[post("/avatars")]
/// async fn upload(form: Multipart) -> Result<NoContent, ApiError> {
///     let image = form.require_file("image")?;
///     let caption = form.text("caption").unwrap_or_default();
///     store(image.file_name(), image.bytes(), caption).await?;
///     Ok(NoContent)
/// }
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Multipart {
    parts: Vec<Part>,
}

impl Multipart {
    /// Every part, in the order they were sent.
    pub fn parts(&self) -> &[Part] {
        &self.parts
    }

    pub fn len(&self) -> usize {
        self.parts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    /// The first part with this name.
    pub fn part(&self, name: &str) -> Option<&Part> {
        self.parts.iter().find(|part| part.name == name)
    }

    /// Every part with this name. HTML forms repeat a name for multi-select
    /// inputs and for `<input type="file" multiple>`.
    pub fn all<'a>(&'a self, name: &str) -> impl Iterator<Item = &'a Part> + 'a {
        // The name is owned so the returned iterator borrows only `self`,
        // rather than tying itself to the caller's string.
        let name = name.to_owned();
        self.parts.iter().filter(move |part| part.name == name)
    }

    /// The text of the first part with this name.
    pub fn text(&self, name: &str) -> Option<&str> {
        self.part(name).and_then(Part::text)
    }

    /// The first *file* part with this name.
    pub fn file(&self, name: &str) -> Option<&Part> {
        self.all(name).find(|part| part.is_file())
    }

    /// Every file part, whatever its name.
    pub fn files(&self) -> impl Iterator<Item = &Part> {
        self.parts.iter().filter(|part| part.is_file())
    }

    /// Like [`Self::text`], but a missing field is a 422 naming it rather than
    /// an `Option` every caller has to unwrap the same way.
    pub fn require_text(&self, name: &str) -> Result<&str, ApiError> {
        match self.part(name) {
            None => Err(missing(name)),
            Some(part) => part.text().ok_or_else(|| {
                ApiError::unprocessable(vec![FieldError::new(
                    format!("/{name}"),
                    "invalid_encoding",
                    "expected UTF-8 text",
                )])
            }),
        }
    }

    /// Like [`Self::file`], with the same treatment for a missing part.
    pub fn require_file(&self, name: &str) -> Result<&Part, ApiError> {
        self.file(name).ok_or_else(|| missing(name))
    }

    /// Deserialises the non-file parts into `T`.
    ///
    /// Repeated names collect into sequences, exactly as they do for a query
    /// string, because both are the same flat key/value shape.
    pub fn deserialize<T: serde::de::DeserializeOwned>(&self) -> Result<T, ApiError> {
        let pairs: Pairs = self
            .parts
            .iter()
            .filter(|part| !part.is_file())
            .filter_map(|part| part.text().map(|text| (part.name.clone(), text.to_owned())))
            .collect();

        pairs.deserialize().map_err(|error| {
            ApiError::unprocessable(vec![FieldError::new("", "invalid_form", error.to_string())])
                .with_title("Invalid form data")
        })
    }

    /// Parses a body against a boundary. Exposed for testing and for callers
    /// doing their own content-type handling.
    pub fn parse(body: &Bytes, boundary: &str) -> Result<Self, MultipartError> {
        parse(body, boundary)
    }
}

fn missing(name: &str) -> ApiError {
    ApiError::unprocessable(vec![FieldError::new(
        format!("/{name}"),
        "missing",
        format!("`{name}` is required"),
    )])
}

/// Why a multipart body could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MultipartError {
    /// The `Content-Type` was not `multipart/form-data`.
    NotMultipart(String),
    /// No `boundary` parameter, or an unusable one.
    MissingBoundary,
    /// The boundary is not a legal one.
    InvalidBoundary(String),
    /// The opening boundary never appeared.
    NoOpeningBoundary,
    /// The body ended before the closing boundary.
    Truncated,
    /// A part had no headers, or they were not terminated.
    MalformedPart,
    /// A part had no `name` in its `Content-Disposition`.
    UnnamedPart,
    /// More than [`MAX_PARTS`] parts.
    TooManyParts { max: usize },
}

impl std::fmt::Display for MultipartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MultipartError::NotMultipart(got) => {
                write!(f, "expected `multipart/form-data`, got `{got}`")
            }
            MultipartError::MissingBoundary => {
                f.write_str("the `Content-Type` has no `boundary` parameter")
            }
            MultipartError::InvalidBoundary(b) => write!(f, "`{b}` is not a valid boundary"),
            MultipartError::NoOpeningBoundary => {
                f.write_str("the body does not start with the declared boundary")
            }
            MultipartError::Truncated => f.write_str("the body ended before the closing boundary"),
            MultipartError::MalformedPart => f.write_str("a part's headers are malformed"),
            MultipartError::UnnamedPart => {
                f.write_str("a part has no `name` in its `Content-Disposition`")
            }
            MultipartError::TooManyParts { max } => {
                write!(f, "more than {max} parts")
            }
        }
    }
}

impl std::error::Error for MultipartError {}

impl From<MultipartError> for ApiError {
    fn from(error: MultipartError) -> Self {
        match error {
            MultipartError::NotMultipart(_) => ApiError::unsupported_media_type(error.to_string()),
            MultipartError::TooManyParts { .. } => ApiError::payload_too_large(error.to_string()),
            other => {
                ApiError::bad_request(other.to_string()).with_title("Malformed multipart body")
            }
        }
    }
}

/// Pulls the `boundary` out of a `multipart/form-data` content type.
pub fn boundary_of(content_type: &str) -> Result<String, MultipartError> {
    let (media_type, rest) = match content_type.split_once(';') {
        Some((media, rest)) => (media.trim(), rest),
        None => (content_type.trim(), ""),
    };

    if !media_type.eq_ignore_ascii_case("multipart/form-data") {
        return Err(MultipartError::NotMultipart(media_type.to_owned()));
    }

    // Quote-aware, because a boundary containing `;` must be quoted and a
    // plain `split(';')` would cut it in half.
    for parameter in split_parameters(rest) {
        let Some((key, value)) = parameter.split_once('=') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("boundary") {
            continue;
        }

        let value = value.trim();
        // A boundary containing a `;` or spaces must be quoted, and browsers
        // quote it even when they need not.
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(value);

        if value.is_empty() || value.len() > 70 {
            // RFC 2046 caps a boundary at 70 characters. Anything longer is
            // either a mistake or an attempt to make scanning expensive.
            return Err(MultipartError::InvalidBoundary(value.to_owned()));
        }
        return Ok(value.to_owned());
    }

    Err(MultipartError::MissingBoundary)
}

/// Finds `needle` in `haystack` starting at `from`.
fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let last = haystack.len() - needle.len();
    (from..=last).find(|&i| &haystack[i..i + needle.len()] == needle)
}

/// Finds the next real delimiter: an occurrence of the boundary marker that is
/// either at the very start of the body or preceded by CRLF.
///
/// The marker can legitimately appear *inside* a part — a text field containing
/// `--boundary`, or arbitrary bytes in an upload — so every match has to be
/// checked, and a failed check has to keep looking rather than conclude the
/// body ended.
fn find_delimiter(body: &[u8], marker: &[u8], from: usize) -> Option<usize> {
    let mut at = from;
    while let Some(index) = find(body, marker, at) {
        if index == 0 || (index >= 2 && &body[index - 2..index] == b"\r\n") {
            return Some(index);
        }
        at = index + 1;
    }
    None
}

fn parse(body: &Bytes, boundary: &str) -> Result<Multipart, MultipartError> {
    let marker = format!("--{boundary}");
    let marker = marker.as_bytes();

    // The opening boundary is either at the very start or preceded by CRLF.
    // Anything before it is the preamble, which RFC 2046 says to ignore.
    let mut cursor = find_delimiter(body, marker, 0).ok_or(MultipartError::NoOpeningBoundary)?;

    let mut parts = Vec::new();

    loop {
        cursor += marker.len();

        // `--` here means this was the closing boundary; the epilogue after it
        // is ignored.
        if body.len() >= cursor + 2 && &body[cursor..cursor + 2] == b"--" {
            return Ok(Multipart { parts });
        }

        // Transport padding: whitespace is permitted between the boundary and
        // the CRLF that ends the line.
        while body.get(cursor).is_some_and(|b| *b == b' ' || *b == b'\t') {
            cursor += 1;
        }

        match body.get(cursor..cursor + 2) {
            Some(b"\r\n") => cursor += 2,
            // A boundary line that neither closes nor is followed by CRLF is a
            // truncated body, not a part.
            _ => return Err(MultipartError::Truncated),
        }

        let next = find_delimiter(body, marker, cursor).ok_or(MultipartError::Truncated)?;

        // The CRLF immediately before the next boundary belongs to the
        // delimiter, not to the part's content.
        let part_end = next - 2;
        let raw = body.slice(cursor..part_end);

        if parts.len() >= MAX_PARTS {
            return Err(MultipartError::TooManyParts { max: MAX_PARTS });
        }
        parts.push(parse_part(&raw)?);

        cursor = next;
    }
}

fn parse_part(raw: &Bytes) -> Result<Part, MultipartError> {
    let split = find(raw, b"\r\n\r\n", 0).ok_or(MultipartError::MalformedPart)?;
    let headers = std::str::from_utf8(&raw[..split]).map_err(|_| MultipartError::MalformedPart)?;
    let data = raw.slice(split + 4..);

    let mut name = None;
    let mut file_name = None;
    let mut content_type = None;

    for line in headers.split("\r\n") {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();

        if key.eq_ignore_ascii_case("content-disposition") {
            for parameter in split_parameters(value) {
                let Some((param, raw_value)) = parameter.split_once('=') else {
                    continue;
                };
                let param = param.trim();
                let unquoted = unquote(raw_value.trim());
                if param.eq_ignore_ascii_case("name") {
                    name = Some(unquoted);
                } else if param.eq_ignore_ascii_case("filename") {
                    file_name = Some(unquoted);
                }
            }
        } else if key.eq_ignore_ascii_case("content-type") {
            content_type = Some(value.trim().to_owned());
        }
    }

    Ok(Part {
        name: name.ok_or(MultipartError::UnnamedPart)?,
        file_name,
        content_type,
        data,
    })
}

/// Splits header parameters on `;`, ignoring separators inside quotes.
///
/// A filename may legitimately contain a semicolon, and a naive `split(';')`
/// would truncate it.
fn split_parameters(value: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut in_quotes = false;
    let mut escaped = false;

    for (index, ch) in value.char_indices() {
        match ch {
            _ if escaped => escaped = false,
            '\\' if in_quotes => escaped = true,
            '"' => in_quotes = !in_quotes,
            ';' if !in_quotes => {
                out.push(&value[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    out.push(&value[start..]);
    out
}

/// Strips surrounding quotes and unescapes what is inside them.
fn unquote(value: &str) -> String {
    let Some(inner) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) else {
        return value.to_owned();
    };

    let mut out = String::with_capacity(inner.len());
    let mut escaped = false;
    for ch in inner.chars() {
        match ch {
            _ if escaped => {
                out.push(ch);
                escaped = false;
            }
            '\\' => escaped = true,
            _ => out.push(ch),
        }
    }
    out
}

impl FromRequest for Multipart {
    async fn from_request(req: &mut Request) -> Result<Self, ApiError> {
        let content_type = req.header(http::header::CONTENT_TYPE).ok_or_else(|| {
            ApiError::unsupported_media_type("Expected a `multipart/form-data` body.")
        })?;

        let boundary = boundary_of(content_type)?;
        Ok(Multipart::parse(req.body(), &boundary)?)
    }
}

impl OperationInput for Multipart {
    fn describe(ctx: &mut OperationContext<'_>) {
        // Without a declared field list the honest description is "an object
        // of strings and files". A handler that wants a precise contract can
        // describe it by implementing `OperationInput` on its own newtype.
        let mut schema = Schema::of_type("object");
        schema.description = Some(
            "Form fields and file uploads. Text parts are strings; file parts \
             are binary."
                .into(),
        );
        schema.additional_properties = Some(AdditionalProperties::Schema(Box::new(Schema {
            any_of: vec![Schema::of_type("string"), Schema::typed("string", "binary")],
            ..Default::default()
        })));

        let mut content = velo_openapi::Map::new();
        content.insert("multipart/form-data".into(), MediaType::new(schema));
        ctx.operation.request_body = Some(RequestBody {
            description: None,
            content,
            required: true,
        });
        ctx.add_problem_response(400, "The multipart body could not be parsed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::test_request;
    use http::StatusCode;

    /// Builds a body with CRLF line endings, which the format requires and
    /// which a raw string literal in a test would not produce.
    fn body(lines: &[&str]) -> Bytes {
        Bytes::from(lines.join("\r\n"))
    }

    fn simple() -> Bytes {
        body(&[
            "--X",
            "Content-Disposition: form-data; name=\"caption\"",
            "",
            "a photo",
            "--X",
            "Content-Disposition: form-data; name=\"image\"; filename=\"cat.png\"",
            "Content-Type: image/png",
            "",
            "\u{0}PNG-BYTES",
            "--X--",
            "",
        ])
    }

    #[test]
    fn text_and_file_parts_are_both_read() {
        let form = Multipart::parse(&simple(), "X").unwrap();
        assert_eq!(form.len(), 2);

        assert_eq!(form.text("caption"), Some("a photo"));
        assert!(!form.part("caption").unwrap().is_file());

        let image = form.file("image").unwrap();
        assert_eq!(image.file_name(), Some("cat.png"));
        assert_eq!(image.content_type(), Some("image/png"));
        assert_eq!(image.bytes(), &Bytes::from("\u{0}PNG-BYTES"));
    }

    #[test]
    fn the_boundary_is_taken_from_the_content_type() {
        assert_eq!(
            boundary_of("multipart/form-data; boundary=abc123").unwrap(),
            "abc123"
        );
        // Browsers quote it; some also vary the case and spacing.
        assert_eq!(
            boundary_of("MULTIPART/FORM-DATA;  BOUNDARY=\"a;b\"").unwrap(),
            "a;b"
        );
    }

    #[test]
    fn a_wrong_or_missing_content_type_is_reported_precisely() {
        assert_eq!(
            boundary_of("application/json"),
            Err(MultipartError::NotMultipart("application/json".into()))
        );
        assert_eq!(
            boundary_of("multipart/form-data"),
            Err(MultipartError::MissingBoundary)
        );
        // RFC 2046 caps a boundary at 70 characters.
        assert!(matches!(
            boundary_of(&format!("multipart/form-data; boundary={}", "x".repeat(71))),
            Err(MultipartError::InvalidBoundary(_))
        ));
    }

    #[test]
    fn binary_content_survives_untouched() {
        // Every byte value, including CR, LF, and the `-` that starts a
        // boundary, must come back exactly as sent.
        let payload: Vec<u8> = (0u8..=255).collect();
        let mut raw = Vec::new();
        raw.extend_from_slice(
            b"--B\r\nContent-Disposition: form-data; name=\"f\"; filename=\"b.bin\"\r\n\r\n",
        );
        raw.extend_from_slice(&payload);
        raw.extend_from_slice(b"\r\n--B--\r\n");

        let form = Multipart::parse(&Bytes::from(raw), "B").unwrap();
        assert_eq!(form.file("f").unwrap().bytes(), &Bytes::from(payload));
    }

    #[test]
    fn a_part_containing_the_boundary_text_is_not_split_early() {
        // `--B` inside the data is only a delimiter when it follows a CRLF.
        let raw = body(&[
            "--B",
            "Content-Disposition: form-data; name=\"t\"",
            "",
            "prefix--B suffix",
            "--B--",
            "",
        ]);
        let form = Multipart::parse(&raw, "B").unwrap();
        assert_eq!(form.len(), 1);
        assert_eq!(form.text("t"), Some("prefix--B suffix"));
    }

    #[test]
    fn a_preamble_and_epilogue_are_ignored() {
        let raw = body(&[
            "This is a preamble that clients are allowed to send.",
            "--B",
            "Content-Disposition: form-data; name=\"t\"",
            "",
            "value",
            "--B--",
            "and an epilogue",
            "",
        ]);
        let form = Multipart::parse(&raw, "B").unwrap();
        assert_eq!(form.len(), 1);
        assert_eq!(form.text("t"), Some("value"));
    }

    #[test]
    fn a_filename_may_contain_quotes_and_semicolons() {
        let raw = body(&[
            "--B",
            r#"Content-Disposition: form-data; name="f"; filename="a;b \"quoted\".txt""#,
            "",
            "x",
            "--B--",
            "",
        ]);
        let form = Multipart::parse(&raw, "B").unwrap();
        assert_eq!(
            form.file("f").unwrap().file_name(),
            Some(r#"a;b "quoted".txt"#)
        );
    }

    #[test]
    fn repeated_names_are_all_kept() {
        let raw = body(&[
            "--B",
            "Content-Disposition: form-data; name=\"tag\"",
            "",
            "one",
            "--B",
            "Content-Disposition: form-data; name=\"tag\"",
            "",
            "two",
            "--B--",
            "",
        ]);
        let form = Multipart::parse(&raw, "B").unwrap();
        let tags: Vec<&str> = form.all("tag").filter_map(Part::text).collect();
        assert_eq!(tags, vec!["one", "two"]);
    }

    #[test]
    fn an_empty_part_is_still_a_part() {
        let raw = body(&[
            "--B",
            "Content-Disposition: form-data; name=\"empty\"",
            "",
            "",
            "--B--",
            "",
        ]);
        let form = Multipart::parse(&raw, "B").unwrap();
        assert_eq!(form.text("empty"), Some(""));
        assert!(form.part("empty").unwrap().is_empty());
    }

    #[test]
    fn a_truncated_body_is_refused_rather_than_half_read() {
        // No closing boundary: the client died mid-upload.
        let raw = body(&[
            "--B",
            "Content-Disposition: form-data; name=\"t\"",
            "",
            "value",
            "",
        ]);
        assert_eq!(Multipart::parse(&raw, "B"), Err(MultipartError::Truncated));
    }

    #[test]
    fn a_body_with_the_wrong_boundary_is_refused() {
        assert_eq!(
            Multipart::parse(&simple(), "OTHER"),
            Err(MultipartError::NoOpeningBoundary)
        );
    }

    #[test]
    fn a_part_without_a_name_is_refused() {
        let raw = body(&[
            "--B",
            "Content-Disposition: form-data",
            "",
            "value",
            "--B--",
            "",
        ]);
        assert_eq!(
            Multipart::parse(&raw, "B"),
            Err(MultipartError::UnnamedPart)
        );
    }

    #[test]
    fn a_part_with_unterminated_headers_is_refused() {
        let raw = Bytes::from("--B\r\nContent-Disposition: form-data; name=\"t\"\r\n--B--\r\n");
        assert_eq!(
            Multipart::parse(&raw, "B"),
            Err(MultipartError::MalformedPart)
        );
    }

    #[test]
    fn part_count_is_capped_independently_of_body_size() {
        let mut raw = String::new();
        for index in 0..MAX_PARTS + 5 {
            raw.push_str(&format!(
                "--B\r\nContent-Disposition: form-data; name=\"f{index}\"\r\n\r\nx\r\n"
            ));
        }
        raw.push_str("--B--\r\n");

        assert_eq!(
            Multipart::parse(&Bytes::from(raw), "B"),
            Err(MultipartError::TooManyParts { max: MAX_PARTS })
        );
    }

    #[test]
    fn text_fields_deserialise_into_a_struct() {
        #[derive(Debug, serde::Deserialize, PartialEq)]
        struct Caption {
            caption: String,
        }

        let form = Multipart::parse(&simple(), "X").unwrap();
        // The file part is skipped; only text fields feed the struct.
        assert_eq!(
            form.deserialize::<Caption>().unwrap(),
            Caption {
                caption: "a photo".into()
            }
        );
    }

    #[test]
    fn requiring_a_missing_field_names_it() {
        let form = Multipart::parse(&simple(), "X").unwrap();
        let error = form.require_text("nope").unwrap_err();
        assert_eq!(error.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(error.field_errors()[0].pointer, "/nope");
        assert_eq!(error.field_errors()[0].code, "missing");
    }

    #[test]
    fn a_text_field_is_not_mistaken_for_a_file() {
        let form = Multipart::parse(&simple(), "X").unwrap();
        assert!(form.file("caption").is_none());
        assert_eq!(form.files().count(), 1);
    }

    #[tokio::test]
    async fn extraction_reads_the_boundary_from_the_request() {
        let mut req = test_request()
            .method("POST")
            .header("content-type", "multipart/form-data; boundary=X")
            .body(simple())
            .build();

        let form = Multipart::from_request(&mut req).await.unwrap();
        assert_eq!(form.text("caption"), Some("a photo"));
    }

    #[tokio::test]
    async fn a_json_body_sent_to_a_multipart_handler_is_a_415() {
        let mut req = test_request()
            .method("POST")
            .header("content-type", "application/json")
            .body("{}")
            .build();

        let error = Multipart::from_request(&mut req).await.unwrap_err();
        assert_eq!(error.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn a_malformed_body_is_a_400_not_a_500() {
        let mut req = test_request()
            .method("POST")
            .header("content-type", "multipart/form-data; boundary=X")
            .body("not multipart at all")
            .build();

        let error = Multipart::from_request(&mut req).await.unwrap_err();
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn the_operation_documents_a_multipart_body() {
        let mut generator = velo_openapi::SchemaGenerator::new();
        let mut operation = velo_openapi::Operation::default();
        let mut ctx = OperationContext {
            generator: &mut generator,
            operation: &mut operation,
            path: "/upload",
            method: "POST",
        };
        Multipart::describe(&mut ctx);

        let body = operation.request_body.as_ref().unwrap();
        assert!(body.required);
        assert!(body.content.contains_key("multipart/form-data"));
        assert!(operation.responses.contains_key("400"));
    }
}
