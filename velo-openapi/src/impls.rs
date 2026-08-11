//! [`JsonSchema`] implementations for the standard library and a few
//! ubiquitous ecosystem types.

use crate::gen::{JsonSchema, SchemaGenerator};
use crate::schema::Schema;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::num::{
    NonZeroI16, NonZeroI32, NonZeroI64, NonZeroI8, NonZeroU16, NonZeroU32, NonZeroU64, NonZeroU8,
};
use std::rc::Rc;
use std::sync::Arc;

macro_rules! simple {
    ($($ty:ty => $schema:expr;)*) => {$(
        impl JsonSchema for $ty {
            fn json_schema(_: &mut SchemaGenerator) -> Schema { $schema }
        }
    )*};
}

simple! {
    bool => Schema::of_type("boolean");
    char => Schema { min_length: Some(1), max_length: Some(1), ..Schema::of_type("string") };
    str => Schema::of_type("string");
    String => Schema::of_type("string");
    f32 => Schema::typed("number", "float");
    f64 => Schema::typed("number", "double");
    i8 => bounded_int("int32", i8::MIN as f64, i8::MAX as f64);
    i16 => bounded_int("int32", i16::MIN as f64, i16::MAX as f64);
    i32 => Schema::typed("integer", "int32");
    i64 => Schema::typed("integer", "int64");
    i128 => Schema::of_type("integer");
    isize => Schema::typed("integer", "int64");
    u8 => bounded_int("int32", 0.0, u8::MAX as f64);
    u16 => bounded_int("int32", 0.0, u16::MAX as f64);
    u32 => bounded_int("int64", 0.0, u32::MAX as f64);
    u64 => Schema { minimum: Some(0.0), ..Schema::typed("integer", "int64") };
    u128 => Schema { minimum: Some(0.0), ..Schema::of_type("integer") };
    usize => Schema { minimum: Some(0.0), ..Schema::typed("integer", "int64") };
    () => Schema::of_type("null");
    std::net::IpAddr => Schema::typed("string", "ip");
    std::net::Ipv4Addr => Schema::typed("string", "ipv4");
    std::net::Ipv6Addr => Schema::typed("string", "ipv6");
    std::path::Path => Schema::of_type("string");
    std::path::PathBuf => Schema::of_type("string");
    std::time::Duration => Schema { minimum: Some(0.0), description: Some("Duration in seconds".into()), ..Schema::typed("number", "double") };
}

fn bounded_int(format: &str, min: f64, max: f64) -> Schema {
    Schema {
        minimum: Some(min),
        maximum: Some(max),
        ..Schema::typed("integer", format)
    }
}

macro_rules! nonzero {
    ($($ty:ty => $inner:ty, $exclusive:expr;)*) => {$(
        impl JsonSchema for $ty {
            fn json_schema(generator: &mut SchemaGenerator) -> Schema {
                let mut s = <$inner as JsonSchema>::json_schema(generator);
                if $exclusive { s.minimum = Some(1.0); } else { s.not = Some(Box::new(Schema { const_value: Some(serde_json::json!(0)), ..Default::default() })); }
                s
            }
        }
    )*};
}

nonzero! {
    NonZeroU8 => u8, true;
    NonZeroU16 => u16, true;
    NonZeroU32 => u32, true;
    NonZeroU64 => u64, true;
    NonZeroI8 => i8, false;
    NonZeroI16 => i16, false;
    NonZeroI32 => i32, false;
    NonZeroI64 => i64, false;
}

impl JsonSchema for serde_json::Value {
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        Schema::any()
    }
}

impl JsonSchema for serde_json::Map<String, serde_json::Value> {
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        Schema::of_type("object")
    }
}

// ---- transparent wrappers -------------------------------------------------

macro_rules! transparent {
    ($($ty:ty),* $(,)?) => {$(
        impl<T: JsonSchema + ?Sized> JsonSchema for $ty {
            fn schema_name() -> Option<String> { T::schema_name() }
            fn json_schema(generator: &mut SchemaGenerator) -> Schema { T::json_schema(generator) }
            const OPTIONAL: bool = T::OPTIONAL;
        }
    )*};
}

transparent!(
    Box<T>,
    Rc<T>,
    Arc<T>,
    std::cell::RefCell<T>,
    std::sync::Mutex<T>
);

impl<'a, T> JsonSchema for Cow<'a, T>
where
    T: JsonSchema + ToOwned + ?Sized,
{
    fn schema_name() -> Option<String> {
        T::schema_name()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        T::json_schema(generator)
    }
    const OPTIONAL: bool = T::OPTIONAL;
}

impl<T: JsonSchema> JsonSchema for Option<T> {
    fn schema_name() -> Option<String> {
        // Deliberately anonymous: `Option<T>` is a use-site modifier, not a
        // distinct component. Nullability is expressed inline.
        None
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        generator.subschema_for::<T>().nullable()
    }
    const OPTIONAL: bool = true;
}

impl<T: JsonSchema, E> JsonSchema for Result<T, E> {
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        generator.subschema_for::<T>()
    }
}

// ---- sequences ------------------------------------------------------------

macro_rules! seq {
    ($($ty:ty),* $(,)?) => {$(
        impl<T: JsonSchema> JsonSchema for $ty {
            fn json_schema(generator: &mut SchemaGenerator) -> Schema {
                Schema::array(generator.subschema_for::<T>())
            }
        }
    )*};
}

macro_rules! unique_seq {
    ($($ty:ty),* $(,)?) => {$(
        impl<T: JsonSchema> JsonSchema for $ty {
            fn json_schema(generator: &mut SchemaGenerator) -> Schema {
                Schema {
                    unique_items: Some(true),
                    ..Schema::array(generator.subschema_for::<T>())
                }
            }
        }
    )*};
}

seq!(Vec<T>, VecDeque<T>, [T]);
unique_seq!(BTreeSet<T>);

impl<T: JsonSchema, S> JsonSchema for HashSet<T, S> {
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let items = generator.subschema_for::<T>();
        Schema {
            unique_items: Some(true),
            ..Schema::array(items)
        }
    }
}

impl<T: JsonSchema, const N: usize> JsonSchema for [T; N] {
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let items = generator.subschema_for::<T>();
        Schema {
            min_items: Some(N as u64),
            max_items: Some(N as u64),
            ..Schema::array(items)
        }
    }
}

// ---- maps -----------------------------------------------------------------

// Keys are always serialised as strings by serde_json, so only the value type
// contributes to the schema.
impl<K, V: JsonSchema, S> JsonSchema for HashMap<K, V, S> {
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        Schema::map_of(generator.subschema_for::<V>())
    }
}

impl<K, V: JsonSchema> JsonSchema for BTreeMap<K, V> {
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        Schema::map_of(generator.subschema_for::<V>())
    }
}

impl<K, V: JsonSchema, S> JsonSchema for indexmap::IndexMap<K, V, S> {
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        Schema::map_of(generator.subschema_for::<V>())
    }
}

// ---- tuples ---------------------------------------------------------------

macro_rules! tuples {
    ($($name:ident),+) => {
        impl<$($name: JsonSchema),+> JsonSchema for ($($name,)+) {
            fn json_schema(generator: &mut SchemaGenerator) -> Schema {
                let prefix_items = vec![$(generator.subschema_for::<$name>()),+];
                let len = prefix_items.len() as u64;
                Schema {
                    schema_type: Some("array".into()),
                    prefix_items,
                    min_items: Some(len),
                    max_items: Some(len),
                    ..Default::default()
                }
            }
        }
    };
}

tuples!(T0);
tuples!(T0, T1);
tuples!(T0, T1, T2);
tuples!(T0, T1, T2, T3);
tuples!(T0, T1, T2, T3, T4);
tuples!(T0, T1, T2, T3, T4, T5);
tuples!(T0, T1, T2, T3, T4, T5, T6);
tuples!(T0, T1, T2, T3, T4, T5, T6, T7);

// ---- ecosystem ------------------------------------------------------------

#[cfg(feature = "uuid")]
impl JsonSchema for uuid::Uuid {
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        Schema {
            examples: vec![serde_json::json!("f8c9d1e2-3a4b-4c5d-8e9f-0a1b2c3d4e5f")],
            ..Schema::typed("string", "uuid")
        }
    }
}

#[cfg(feature = "chrono")]
mod chrono_impls {
    use super::*;

    impl<Tz: chrono::TimeZone> JsonSchema for chrono::DateTime<Tz> {
        fn json_schema(_: &mut SchemaGenerator) -> Schema {
            Schema::typed("string", "date-time")
        }
    }
    impl JsonSchema for chrono::NaiveDate {
        fn json_schema(_: &mut SchemaGenerator) -> Schema {
            Schema::typed("string", "date")
        }
    }
    impl JsonSchema for chrono::NaiveTime {
        fn json_schema(_: &mut SchemaGenerator) -> Schema {
            Schema::typed("string", "time")
        }
    }
    impl JsonSchema for chrono::NaiveDateTime {
        fn json_schema(_: &mut SchemaGenerator) -> Schema {
            Schema::typed("string", "date-time")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::schema_for;

    #[test]
    fn option_is_nullable_and_not_required() {
        const _: () = assert!(<Option<String> as JsonSchema>::OPTIONAL);
        const _: () = assert!(!<String as JsonSchema>::OPTIONAL);
        let (s, _) = schema_for::<Option<String>>();
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["type"], serde_json::json!(["string", "null"]));
    }

    #[test]
    fn u8_carries_its_real_bounds() {
        let (s, _) = schema_for::<u8>();
        assert_eq!(s.minimum, Some(0.0));
        assert_eq!(s.maximum, Some(255.0));
    }

    #[test]
    fn sets_are_unique_arrays() {
        let (s, _) = schema_for::<HashSet<String>>();
        assert_eq!(s.unique_items, Some(true));
    }

    #[test]
    fn fixed_arrays_pin_their_length() {
        let (s, _) = schema_for::<[u32; 3]>();
        assert_eq!((s.min_items, s.max_items), (Some(3), Some(3)));
    }

    #[test]
    fn tuples_use_prefix_items() {
        let (s, _) = schema_for::<(String, u32)>();
        assert_eq!(s.prefix_items.len(), 2);
        assert_eq!(s.max_items, Some(2));
    }
}
