//! Declarative validation.
//!
//! A `#[validate(...)]` attribute on a `#[derive(Schema)]` field does two
//! things from one declaration: it emits the matching JSON Schema keyword, and
//! it generates a runtime check. That is the property FastAPI gets right and
//! most Rust frameworks get wrong — there is no way for the documented
//! constraint and the enforced constraint to disagree, because they are the
//! same annotation.

use crate::error::{ApiError, FieldError};

/// Accumulated field-level failures.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ValidationErrors {
    errors: Vec<FieldError>,
}

impl ValidationErrors {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a failure at `pointer`.
    pub fn push(
        &mut self,
        pointer: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.errors.push(FieldError::new(pointer, code, message));
    }

    /// Merges nested errors, prefixing their pointers with `prefix`.
    pub fn merge_at(&mut self, prefix: &str, other: ValidationErrors) {
        self.errors.extend(other.errors.into_iter().map(|mut e| {
            e.pointer = format!("{prefix}{}", e.pointer);
            e
        }));
    }

    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }

    pub fn len(&self) -> usize {
        self.errors.len()
    }

    pub fn as_slice(&self) -> &[FieldError] {
        &self.errors
    }

    pub fn into_inner(self) -> Vec<FieldError> {
        self.errors
    }

    /// `Ok(())` when nothing failed, otherwise `Err(self)`.
    pub fn into_result(self) -> Result<(), ValidationErrors> {
        if self.is_empty() {
            Ok(())
        } else {
            Err(self)
        }
    }
}

impl std::fmt::Display for ValidationErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut first = true;
        for error in &self.errors {
            if !first {
                f.write_str("; ")?;
            }
            first = false;
            write!(f, "{}: {}", error.pointer, error.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for ValidationErrors {}

impl From<ValidationErrors> for ApiError {
    fn from(errors: ValidationErrors) -> Self {
        ApiError::unprocessable(errors.into_inner())
    }
}

/// A type whose values can be checked beyond what deserialisation guarantees.
///
/// `#[derive(Schema)]` implements this for you. The blanket implementations
/// below make every std container transparently validatable, so
/// `#[validate(nested)]` works through `Vec`, `Option`, and maps.
pub trait Validate {
    /// Checks `self`, reporting failures with pointers relative to `self`.
    fn validate(&self) -> Result<(), ValidationErrors> {
        Ok(())
    }
}

macro_rules! no_op_validate {
    ($($ty:ty),* $(,)?) => {$( impl Validate for $ty {} )*};
}

no_op_validate!(
    (),
    bool,
    char,
    str,
    String,
    f32,
    f64,
    i8,
    i16,
    i32,
    i64,
    i128,
    isize,
    u8,
    u16,
    u32,
    u64,
    u128,
    usize,
    serde_json::Value,
    std::net::IpAddr,
    std::net::Ipv4Addr,
    std::net::Ipv6Addr,
    std::path::PathBuf,
    std::time::Duration,
);

#[cfg(feature = "uuid")]
impl Validate for uuid::Uuid {}

#[cfg(feature = "chrono")]
mod chrono_validate {
    use super::Validate;
    impl<Tz: chrono::TimeZone> Validate for chrono::DateTime<Tz> {}
    impl Validate for chrono::NaiveDate {}
    impl Validate for chrono::NaiveTime {}
    impl Validate for chrono::NaiveDateTime {}
}

impl<T: Validate + ?Sized> Validate for Box<T> {
    fn validate(&self) -> Result<(), ValidationErrors> {
        (**self).validate()
    }
}

impl<T: Validate + ?Sized> Validate for std::sync::Arc<T> {
    fn validate(&self) -> Result<(), ValidationErrors> {
        (**self).validate()
    }
}

impl<T: Validate> Validate for Option<T> {
    fn validate(&self) -> Result<(), ValidationErrors> {
        match self {
            Some(value) => value.validate(),
            None => Ok(()),
        }
    }
}

macro_rules! indexed_validate {
    ($($ty:ty),* $(,)?) => {$(
        impl<T: Validate> Validate for $ty {
            fn validate(&self) -> Result<(), ValidationErrors> {
                let mut errors = ValidationErrors::new();
                for (index, item) in self.iter().enumerate() {
                    if let Err(nested) = item.validate() {
                        errors.merge_at(&format!("/{index}"), nested);
                    }
                }
                errors.into_result()
            }
        }
    )*};
}

indexed_validate!(Vec<T>, [T], std::collections::VecDeque<T>);

impl<K: std::fmt::Display, V: Validate, S> Validate for std::collections::HashMap<K, V, S> {
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::new();
        for (key, value) in self.iter() {
            if let Err(nested) = value.validate() {
                errors.merge_at(&format!("/{}", escape_pointer(&key.to_string())), nested);
            }
        }
        errors.into_result()
    }
}

impl<K: std::fmt::Display, V: Validate> Validate for std::collections::BTreeMap<K, V> {
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::new();
        for (key, value) in self.iter() {
            if let Err(nested) = value.validate() {
                errors.merge_at(&format!("/{}", escape_pointer(&key.to_string())), nested);
            }
        }
        errors.into_result()
    }
}

/// Escapes `~` and `/` per RFC 6901 so map keys cannot forge pointer segments.
fn escape_pointer(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

/// The checks a `#[validate(...)]` attribute expands to.
///
/// These are called by generated code and are not usually written by hand,
/// but they are public so hand-written `Validate` impls can reuse them.
pub mod rules {
    use super::ValidationErrors;

    /// Anything with a meaningful length: strings count characters, containers
    /// count elements.
    pub trait Length {
        fn measure(&self) -> usize;
        /// What the length is counting, used in messages.
        fn unit() -> &'static str {
            "items"
        }
    }

    impl Length for str {
        fn measure(&self) -> usize {
            // Characters, not bytes: JSON Schema counts code points, and a
            // byte count would reject valid non-ASCII input.
            self.chars().count()
        }
        fn unit() -> &'static str {
            "characters"
        }
    }

    impl Length for String {
        fn measure(&self) -> usize {
            self.chars().count()
        }
        fn unit() -> &'static str {
            "characters"
        }
    }

    impl<T> Length for Vec<T> {
        fn measure(&self) -> usize {
            self.len()
        }
    }

    impl<T> Length for [T] {
        fn measure(&self) -> usize {
            self.len()
        }
    }

    impl<K, V, S> Length for std::collections::HashMap<K, V, S> {
        fn measure(&self) -> usize {
            self.len()
        }
        fn unit() -> &'static str {
            "entries"
        }
    }

    impl<T, S> Length for std::collections::HashSet<T, S> {
        fn measure(&self) -> usize {
            self.len()
        }
    }

    impl<K, V> Length for std::collections::BTreeMap<K, V> {
        fn measure(&self) -> usize {
            self.len()
        }
        fn unit() -> &'static str {
            "entries"
        }
    }

    impl<T> Length for std::collections::BTreeSet<T> {
        fn measure(&self) -> usize {
            self.len()
        }
    }

    /// A numeric value comparable against a bound.
    pub trait Number {
        fn as_f64(&self) -> f64;
    }

    macro_rules! number {
        ($($ty:ty),*) => {$( impl Number for $ty { fn as_f64(&self) -> f64 { *self as f64 } } )*};
    }
    number!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize, f32, f64);

    pub fn min_length<T: Length + ?Sized>(
        value: &T,
        min: usize,
        pointer: &str,
        errors: &mut ValidationErrors,
    ) {
        let actual = value.measure();
        if actual < min {
            errors.push(
                pointer,
                "min_length",
                format!(
                    "must be at least {min} {}, got {actual}",
                    <T as Length>::unit()
                ),
            );
        }
    }

    pub fn max_length<T: Length + ?Sized>(
        value: &T,
        max: usize,
        pointer: &str,
        errors: &mut ValidationErrors,
    ) {
        let actual = value.measure();
        if actual > max {
            errors.push(
                pointer,
                "max_length",
                format!(
                    "must be at most {max} {}, got {actual}",
                    <T as Length>::unit()
                ),
            );
        }
    }

    pub fn min_items<T: Length + ?Sized>(
        value: &T,
        min: usize,
        pointer: &str,
        errors: &mut ValidationErrors,
    ) {
        if value.measure() < min {
            errors.push(
                pointer,
                "min_items",
                format!("must have at least {min} items"),
            );
        }
    }

    pub fn max_items<T: Length + ?Sized>(
        value: &T,
        max: usize,
        pointer: &str,
        errors: &mut ValidationErrors,
    ) {
        if value.measure() > max {
            errors.push(
                pointer,
                "max_items",
                format!("must have at most {max} items"),
            );
        }
    }

    pub fn minimum<T: Number + ?Sized>(
        value: &T,
        min: f64,
        pointer: &str,
        errors: &mut ValidationErrors,
    ) {
        if value.as_f64() < min {
            errors.push(pointer, "minimum", format!("must be >= {min}"));
        }
    }

    pub fn maximum<T: Number + ?Sized>(
        value: &T,
        max: f64,
        pointer: &str,
        errors: &mut ValidationErrors,
    ) {
        if value.as_f64() > max {
            errors.push(pointer, "maximum", format!("must be <= {max}"));
        }
    }

    pub fn exclusive_minimum<T: Number + ?Sized>(
        value: &T,
        min: f64,
        pointer: &str,
        errors: &mut ValidationErrors,
    ) {
        if value.as_f64() <= min {
            errors.push(pointer, "exclusive_minimum", format!("must be > {min}"));
        }
    }

    pub fn exclusive_maximum<T: Number + ?Sized>(
        value: &T,
        max: f64,
        pointer: &str,
        errors: &mut ValidationErrors,
    ) {
        if value.as_f64() >= max {
            errors.push(pointer, "exclusive_maximum", format!("must be < {max}"));
        }
    }

    pub fn multiple_of<T: Number + ?Sized>(
        value: &T,
        divisor: f64,
        pointer: &str,
        errors: &mut ValidationErrors,
    ) {
        let value = value.as_f64();
        if divisor == 0.0 || (value / divisor).fract().abs() > f64::EPSILON {
            errors.push(
                pointer,
                "multiple_of",
                format!("must be a multiple of {divisor}"),
            );
        }
    }

    /// Rejects strings that are empty or entirely whitespace. `min_length = 1`
    /// accepts `" "`, which is almost never what the author meant.
    pub fn non_blank(value: &str, pointer: &str, errors: &mut ValidationErrors) {
        if value.trim().is_empty() {
            errors.push(pointer, "non_blank", "must not be blank");
        }
    }

    pub fn contains(value: &str, needle: &str, pointer: &str, errors: &mut ValidationErrors) {
        if !value.contains(needle) {
            errors.push(pointer, "contains", format!("must contain {needle:?}"));
        }
    }

    pub fn one_of<T: PartialEq + std::fmt::Debug>(
        value: &T,
        allowed: &[T],
        pointer: &str,
        errors: &mut ValidationErrors,
    ) {
        if !allowed.contains(value) {
            errors.push(pointer, "one_of", format!("must be one of {allowed:?}"));
        }
    }

    #[cfg(feature = "regex")]
    pub fn pattern(value: &str, regex: &str, pointer: &str, errors: &mut ValidationErrors) {
        // Compiling per call is wasteful; the derive hoists this into a
        // `OnceLock` so each pattern compiles once per process.
        match regex::Regex::new(regex) {
            Ok(re) if re.is_match(value) => {}
            Ok(_) => errors.push(pointer, "pattern", format!("must match /{regex}/")),
            Err(_) => errors.push(pointer, "pattern", "server-side pattern is invalid"),
        }
    }

    #[cfg(feature = "regex")]
    pub fn pattern_compiled(
        value: &str,
        regex: &regex::Regex,
        pointer: &str,
        errors: &mut ValidationErrors,
    ) {
        if !regex.is_match(value) {
            errors.push(
                pointer,
                "pattern",
                format!("must match /{}/", regex.as_str()),
            );
        }
    }

    /// Validates a `format` keyword. Unknown formats pass, matching JSON
    /// Schema's annotation-by-default semantics.
    pub fn format(value: &str, format: &str, pointer: &str, errors: &mut ValidationErrors) {
        let ok = match format {
            "email" => is_email(value),
            "uri" | "url" => is_uri(value),
            "uuid" => is_uuid(value),
            "ipv4" => value.parse::<std::net::Ipv4Addr>().is_ok(),
            "ipv6" => value.parse::<std::net::Ipv6Addr>().is_ok(),
            "hostname" => is_hostname(value),
            "date-time" => is_rfc3339(value),
            _ => true,
        };
        if !ok {
            errors.push(pointer, "format", format!("must be a valid {format}"));
        }
    }

    /// A deliberately structural email check.
    ///
    /// Regex-based email validation is famously either wrong or unreadable;
    /// this rejects what is unambiguously invalid and leaves the rest to a
    /// confirmation mail, which is the only real check anyway.
    pub fn is_email(value: &str) -> bool {
        let mut parts = value.split('@');
        let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
            return false;
        };
        !local.is_empty()
            && local.len() <= 64
            && !local.starts_with('.')
            && !local.ends_with('.')
            && !local.contains("..")
            && is_hostname(domain)
            && domain.contains('.')
    }

    pub fn is_hostname(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 253
            && !value.starts_with('.')
            && !value.ends_with('.')
            && value.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            })
    }

    pub fn is_uri(value: &str) -> bool {
        let Some((scheme, rest)) = value.split_once(':') else {
            return false;
        };
        !rest.is_empty()
            && !scheme.is_empty()
            && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    }

    pub fn is_uuid(value: &str) -> bool {
        let bytes = value.as_bytes();
        bytes.len() == 36
            && bytes.iter().enumerate().all(|(i, &b)| match i {
                8 | 13 | 18 | 23 => b == b'-',
                _ => b.is_ascii_hexdigit(),
            })
    }

    pub fn is_rfc3339(value: &str) -> bool {
        // Cheap shape check: YYYY-MM-DDT..
        let bytes = value.as_bytes();
        bytes.len() >= 20
            && bytes[..4].iter().all(u8::is_ascii_digit)
            && bytes[4] == b'-'
            && bytes[7] == b'-'
            && (bytes[10] == b'T' || bytes[10] == b't' || bytes[10] == b' ')
            && bytes[13] == b':'
            && bytes[16] == b':'
    }
}

#[cfg(test)]
mod tests {
    use super::rules::*;
    use super::*;

    #[test]
    fn length_counts_characters_not_bytes() {
        let mut errors = ValidationErrors::new();
        // 3 characters, 9 bytes. A byte-based check would wrongly reject it.
        min_length("日本語", 3, "/name", &mut errors);
        assert!(errors.is_empty());
        max_length("日本語", 3, "/name", &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn failures_carry_pointer_code_and_message() {
        let mut errors = ValidationErrors::new();
        min_length("ab", 5, "/name", &mut errors);
        let error = &errors.as_slice()[0];
        assert_eq!(error.pointer, "/name");
        assert_eq!(error.code, "min_length");
        assert!(error.message.contains("at least 5 characters"));
    }

    #[test]
    fn nested_errors_are_prefixed() {
        let mut outer = ValidationErrors::new();
        let mut inner = ValidationErrors::new();
        inner.push("/street", "min_length", "too short");
        outer.merge_at("/address", inner);
        assert_eq!(outer.as_slice()[0].pointer, "/address/street");
    }

    #[test]
    fn vec_validation_indexes_each_element() {
        struct Item(u32);
        impl Validate for Item {
            fn validate(&self) -> Result<(), ValidationErrors> {
                let mut errors = ValidationErrors::new();
                minimum(&self.0, 10.0, "", &mut errors);
                errors.into_result()
            }
        }
        let errors = vec![Item(50), Item(1)].validate().unwrap_err();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors.as_slice()[0].pointer, "/1");
    }

    #[test]
    fn none_is_always_valid() {
        struct Never;
        impl Validate for Never {
            fn validate(&self) -> Result<(), ValidationErrors> {
                let mut errors = ValidationErrors::new();
                errors.push("", "always", "nope");
                errors.into_result()
            }
        }
        assert!(Option::<Never>::None.validate().is_ok());
        assert!(Some(Never).validate().is_err());
    }

    #[test]
    fn map_keys_are_pointer_escaped() {
        struct Bad;
        impl Validate for Bad {
            fn validate(&self) -> Result<(), ValidationErrors> {
                let mut errors = ValidationErrors::new();
                errors.push("", "x", "y");
                errors.into_result()
            }
        }
        let mut map = std::collections::BTreeMap::new();
        map.insert("a/b".to_owned(), Bad);
        let errors = map.validate().unwrap_err();
        assert_eq!(errors.as_slice()[0].pointer, "/a~1b");
    }

    #[test]
    fn non_blank_rejects_whitespace_that_min_length_accepts() {
        let mut errors = ValidationErrors::new();
        min_length("   ", 1, "/x", &mut errors);
        assert!(errors.is_empty());
        non_blank("   ", "/x", &mut errors);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn email_check_accepts_real_addresses_and_rejects_broken_ones() {
        for good in [
            "a@b.co",
            "first.last+tag@sub.example.com",
            "x_y@example-1.org",
        ] {
            assert!(is_email(good), "should accept {good}");
        }
        for bad in [
            "no-at-sign",
            "@example.com",
            "a@",
            "a@b",
            "a@@b.com",
            ".a@b.com",
            "a..b@c.com",
            "a@-b.com",
        ] {
            assert!(!is_email(bad), "should reject {bad}");
        }
    }

    #[test]
    fn unknown_formats_are_annotations_not_failures() {
        let mut errors = ValidationErrors::new();
        format("anything", "credit-card", "/x", &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn validation_errors_become_a_422() {
        let mut errors = ValidationErrors::new();
        errors.push("/age", "minimum", "must be >= 13");
        let api_error: ApiError = errors.into();
        assert_eq!(api_error.status(), http::StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(api_error.field_errors().len(), 1);
    }

    #[test]
    fn multiple_of_accepts_exact_multiples_only() {
        let mut errors = ValidationErrors::new();
        multiple_of(&15u32, 5.0, "/x", &mut errors);
        assert!(errors.is_empty());
        multiple_of(&16u32, 5.0, "/x", &mut errors);
        assert_eq!(errors.len(), 1);
    }
}
