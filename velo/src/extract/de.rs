//! A serde deserializer for the flat, stringly-typed key/value data that
//! arrives in query strings, form bodies, and path segments.
//!
//! `serde_urlencoded` cannot represent repeated keys, so `?tag=a&tag=b` fails
//! to deserialise into `Vec<String>` — a real limitation given how common that
//! shape is. This deserializer groups repeated keys instead, so sequences,
//! options, and scalars all behave the way callers expect.

use serde::de::{
    self, DeserializeSeed, Deserializer, EnumAccess, IntoDeserializer, MapAccess, SeqAccess,
    Unexpected, VariantAccess, Visitor,
};
use serde::forward_to_deserialize_any;
use std::fmt;

/// A deserialisation failure, carrying a message suitable for a 422 response.
#[derive(Clone, Debug, PartialEq)]
pub struct DeError(pub String);

impl fmt::Display for DeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DeError {}

impl de::Error for DeError {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        DeError(msg.to_string())
    }
}

/// Key/value pairs with repeats grouped under one key, order preserved.
#[derive(Clone, Debug, Default)]
pub struct Pairs {
    entries: Vec<(String, Vec<String>)>,
}

impl Pairs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a value, appending to an existing key rather than replacing it.
    pub fn push(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        let value = value.into();
        match self.entries.iter_mut().find(|(k, _)| *k == key) {
            Some((_, values)) => values.push(value),
            None => self.entries.push((key, vec![value])),
        }
    }

    /// Parses `a=1&b=2&a=3`, percent-decoding keys and values and treating
    /// `+` as a space.
    pub fn parse_urlencoded(input: &str) -> Self {
        let mut pairs = Pairs::new();
        for segment in input.split('&').filter(|s| !s.is_empty()) {
            let (raw_key, raw_value) = match segment.split_once('=') {
                Some((k, v)) => (k, v),
                // A bare key (`?flag`) is a present-but-empty value, which is
                // how HTML forms encode checkboxes and how flags read best.
                None => (segment, ""),
            };
            pairs.push(decode(raw_key), decode(raw_value));
        }
        pairs
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(k, _)| k.as_str())
    }

    /// The first value recorded for `key`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| v.first())
            .map(String::as_str)
    }

    /// Deserialises into `T`.
    pub fn deserialize<T: serde::de::DeserializeOwned>(self) -> Result<T, DeError> {
        T::deserialize(PairsDeserializer::new(self))
    }
}

impl FromIterator<(String, String)> for Pairs {
    fn from_iter<I: IntoIterator<Item = (String, String)>>(iter: I) -> Self {
        let mut pairs = Pairs::new();
        for (k, v) in iter {
            pairs.push(k, v);
        }
        pairs
    }
}

/// Percent-decodes and converts `+` to space.
fn decode(input: &str) -> String {
    let replaced = if input.contains('+') {
        input.replace('+', " ")
    } else {
        input.to_owned()
    };
    percent_encoding::percent_decode_str(&replaced)
        .decode_utf8_lossy()
        .into_owned()
}

/// Deserialises [`Pairs`] as a map, a sequence, or a lone scalar.
pub struct PairsDeserializer {
    pairs: Pairs,
}

impl PairsDeserializer {
    pub fn new(pairs: Pairs) -> Self {
        Self { pairs }
    }

    /// Scalars are only meaningful when exactly one value was supplied, which
    /// is the `Path<u32>` case.
    fn sole_value(&self) -> Result<&str, DeError> {
        match self.pairs.entries.as_slice() {
            [(_, values)] if values.len() == 1 => Ok(&values[0]),
            other => Err(DeError(format!(
                "expected exactly one value, found {}",
                other.len()
            ))),
        }
    }
}

macro_rules! deserialize_scalar {
    ($($method:ident => $visit:ident : $ty:ty),* $(,)?) => {$(
        fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
            let raw = self.sole_value()?;
            let parsed = raw.parse::<$ty>().map_err(|_| {
                DeError(format!("`{raw}` is not a valid {}", stringify!($ty)))
            })?;
            visitor.$visit(parsed)
        }
    )*};
}

impl<'de> Deserializer<'de> for PairsDeserializer {
    type Error = DeError;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_map(visitor)
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_map(PairsMapAccess {
            entries: self.pairs.entries.into_iter(),
            pending: None,
        })
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.deserialize_map(visitor)
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        // Positional data: every value in order, ignoring keys. This is what
        // `Path<(u32, String)>` needs.
        let flat: Vec<String> = self
            .pairs
            .entries
            .into_iter()
            .flat_map(|(_, values)| values)
            .collect();
        visitor.visit_seq(ValuesSeqAccess {
            values: flat.into_iter(),
        })
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.deserialize_seq(visitor)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.deserialize_seq(visitor)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        if self.pairs.is_empty() {
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }

    fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_unit()
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_str(self.sole_value()?)
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_str(visitor)
    }

    fn deserialize_char<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let raw = self.sole_value()?;
        let mut chars = raw.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => visitor.visit_char(c),
            _ => Err(DeError(format!("`{raw}` is not a single character"))),
        }
    }

    fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_bool(parse_bool(self.sole_value()?)?)
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_bytes(self.sole_value()?.as_bytes())
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_bytes(visitor)
    }

    fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_str(visitor)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        visitor.visit_enum(UnitVariantAccess {
            value: self.sole_value()?.to_owned(),
        })
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        visitor.visit_unit()
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_unit()
    }

    deserialize_scalar! {
        deserialize_i8 => visit_i8: i8,
        deserialize_i16 => visit_i16: i16,
        deserialize_i32 => visit_i32: i32,
        deserialize_i64 => visit_i64: i64,
        deserialize_u8 => visit_u8: u8,
        deserialize_u16 => visit_u16: u16,
        deserialize_u32 => visit_u32: u32,
        deserialize_u64 => visit_u64: u64,
        deserialize_f32 => visit_f32: f32,
        deserialize_f64 => visit_f64: f64,
    }
}

struct PairsMapAccess {
    entries: std::vec::IntoIter<(String, Vec<String>)>,
    pending: Option<Vec<String>>,
}

impl<'de> MapAccess<'de> for PairsMapAccess {
    type Error = DeError;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Self::Error> {
        match self.entries.next() {
            Some((key, values)) => {
                self.pending = Some(values);
                seed.deserialize(key.into_deserializer()).map(Some)
            }
            None => Ok(None),
        }
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, Self::Error> {
        let values = self
            .pending
            .take()
            .ok_or_else(|| DeError("value requested before key".into()))?;
        seed.deserialize(ValuesDeserializer { values })
    }
}

/// Deserialises the group of values recorded under a single key.
struct ValuesDeserializer {
    values: Vec<String>,
}

impl ValuesDeserializer {
    fn first(&self) -> Result<&str, DeError> {
        self.values
            .first()
            .map(String::as_str)
            .ok_or_else(|| DeError("missing value".into()))
    }
}

macro_rules! values_scalar {
    ($($method:ident => $visit:ident : $ty:ty),* $(,)?) => {$(
        fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
            let raw = self.first()?;
            let parsed = raw.parse::<$ty>().map_err(|_| {
                DeError(format!("`{raw}` is not a valid {}", stringify!($ty)))
            })?;
            visitor.$visit(parsed)
        }
    )*};
}

impl<'de> Deserializer<'de> for ValuesDeserializer {
    type Error = DeError;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        // Without type information, a single value is a string and repeats are
        // a list. This keeps `HashMap<String, Value>`-style targets usable.
        if self.values.len() == 1 {
            visitor.visit_str(&self.values[0])
        } else {
            self.deserialize_seq(visitor)
        }
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_seq(ValuesSeqAccess {
            values: self.values.into_iter(),
        })
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        // An explicitly empty value (`?q=`) means "absent" for optional fields,
        // which is how browsers submit untouched form inputs.
        match self.values.first() {
            None => visitor.visit_none(),
            Some(v) if v.is_empty() && self.values.len() == 1 => visitor.visit_none(),
            Some(_) => visitor.visit_some(self),
        }
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_str(self.first()?)
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_str(visitor)
    }

    fn deserialize_char<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let raw = self.first()?;
        let mut chars = raw.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => visitor.visit_char(c),
            _ => Err(DeError(format!("`{raw}` is not a single character"))),
        }
    }

    fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_bool(parse_bool(self.first()?)?)
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_bytes(self.first()?.as_bytes())
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_bytes(visitor)
    }

    fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_unit()
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        visitor.visit_unit()
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        visitor.visit_enum(UnitVariantAccess {
            value: self.first()?.to_owned(),
        })
    }

    fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_str(visitor)
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_unit()
    }

    values_scalar! {
        deserialize_i8 => visit_i8: i8,
        deserialize_i16 => visit_i16: i16,
        deserialize_i32 => visit_i32: i32,
        deserialize_i64 => visit_i64: i64,
        deserialize_u8 => visit_u8: u8,
        deserialize_u16 => visit_u16: u16,
        deserialize_u32 => visit_u32: u32,
        deserialize_u64 => visit_u64: u64,
        deserialize_f32 => visit_f32: f32,
        deserialize_f64 => visit_f64: f64,
    }

    forward_to_deserialize_any! { map struct tuple tuple_struct }
}

struct ValuesSeqAccess {
    values: std::vec::IntoIter<String>,
}

impl<'de> SeqAccess<'de> for ValuesSeqAccess {
    type Error = DeError;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Self::Error> {
        match self.values.next() {
            Some(value) => seed
                .deserialize(ValuesDeserializer {
                    values: vec![value],
                })
                .map(Some),
            None => Ok(None),
        }
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.values.len())
    }
}

/// Enum support is limited to unit variants, which is all a flat string
/// encoding can express.
struct UnitVariantAccess {
    value: String,
}

impl<'de> EnumAccess<'de> for UnitVariantAccess {
    type Error = DeError;
    type Variant = Self;

    fn variant_seed<V: DeserializeSeed<'de>>(
        self,
        seed: V,
    ) -> Result<(V::Value, Self::Variant), Self::Error> {
        let variant = seed.deserialize(self.value.clone().into_deserializer())?;
        Ok((variant, self))
    }
}

impl<'de> VariantAccess<'de> for UnitVariantAccess {
    type Error = DeError;

    fn unit_variant(self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(
        self,
        _seed: T,
    ) -> Result<T::Value, Self::Error> {
        Err(de::Error::invalid_type(
            Unexpected::UnitVariant,
            &"a unit variant",
        ))
    }

    fn tuple_variant<V: Visitor<'de>>(
        self,
        _len: usize,
        _visitor: V,
    ) -> Result<V::Value, Self::Error> {
        Err(de::Error::invalid_type(
            Unexpected::UnitVariant,
            &"a unit variant",
        ))
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        _fields: &'static [&'static str],
        _visitor: V,
    ) -> Result<V::Value, Self::Error> {
        Err(de::Error::invalid_type(
            Unexpected::UnitVariant,
            &"a unit variant",
        ))
    }
}

fn parse_bool(raw: &str) -> Result<bool, DeError> {
    match raw {
        "true" | "1" | "on" | "yes" => Ok(true),
        "false" | "0" | "off" | "no" => Ok(false),
        // A bare `?flag` with no value reads as enabling the flag.
        "" => Ok(true),
        other => Err(DeError(format!("`{other}` is not a valid boolean"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Filters {
        q: String,
        #[serde(default)]
        tag: Vec<String>,
        page: Option<u32>,
        #[serde(default)]
        verbose: bool,
    }

    #[test]
    fn repeated_keys_become_a_vec() {
        let pairs = Pairs::parse_urlencoded("q=rust&tag=web&tag=async&page=3");
        let filters: Filters = pairs.deserialize().unwrap();
        assert_eq!(filters.q, "rust");
        assert_eq!(filters.tag, vec!["web", "async"]);
        assert_eq!(filters.page, Some(3));
    }

    #[test]
    fn missing_optional_fields_are_none() {
        let filters: Filters = Pairs::parse_urlencoded("q=rust").deserialize().unwrap();
        assert_eq!(filters.page, None);
        assert!(filters.tag.is_empty());
    }

    #[test]
    fn empty_value_reads_as_absent_for_options() {
        let filters: Filters = Pairs::parse_urlencoded("q=rust&page=")
            .deserialize()
            .unwrap();
        assert_eq!(filters.page, None);
    }

    #[test]
    fn bare_key_enables_a_bool_flag() {
        let filters: Filters = Pairs::parse_urlencoded("q=rust&verbose")
            .deserialize()
            .unwrap();
        assert!(filters.verbose);
    }

    #[test]
    fn bools_accept_the_usual_spellings() {
        for (input, expected) in [("on", true), ("1", true), ("no", false), ("false", false)] {
            let filters: Filters = Pairs::parse_urlencoded(&format!("q=x&verbose={input}"))
                .deserialize()
                .unwrap();
            assert_eq!(filters.verbose, expected, "for {input}");
        }
    }

    #[test]
    fn percent_and_plus_encoding_are_decoded() {
        let pairs = Pairs::parse_urlencoded("q=hello+world&tag=a%2Fb");
        assert_eq!(pairs.get("q"), Some("hello world"));
        assert_eq!(pairs.get("tag"), Some("a/b"));
    }

    #[test]
    fn bad_numbers_name_the_offending_value() {
        let err = Pairs::parse_urlencoded("q=x&page=abc")
            .deserialize::<Filters>()
            .unwrap_err();
        assert!(err.0.contains("abc"), "unhelpful message: {}", err.0);
    }

    #[test]
    fn a_lone_pair_deserialises_as_a_scalar() {
        let mut pairs = Pairs::new();
        pairs.push("id", "42");
        assert_eq!(pairs.clone().deserialize::<u32>().unwrap(), 42);
        assert_eq!(pairs.deserialize::<String>().unwrap(), "42");
    }

    #[test]
    fn ordered_pairs_deserialise_as_a_tuple() {
        let mut pairs = Pairs::new();
        pairs.push("org", "acme");
        pairs.push("id", "7");
        let (org, id): (String, u32) = pairs.deserialize().unwrap();
        assert_eq!((org.as_str(), id), ("acme", 7));
    }

    #[test]
    fn unit_enums_round_trip() {
        #[derive(Debug, Deserialize, PartialEq)]
        #[serde(rename_all = "lowercase")]
        enum Sort {
            Asc,
            Desc,
        }
        #[derive(Debug, Deserialize, PartialEq)]
        struct Q {
            sort: Sort,
        }
        let q: Q = Pairs::parse_urlencoded("sort=desc").deserialize().unwrap();
        assert_eq!(q.sort, Sort::Desc);
    }
}
