use std::{collections::BTreeMap, fmt, marker::PhantomData};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// Reject repeated JSON keys before inserting them into an ordered map.
pub(crate) fn ordered_map<'de, D, V>(deserializer: D) -> Result<BTreeMap<String, V>, D::Error>
where
    D: Deserializer<'de>,
    V: Deserialize<'de>,
{
    struct UniqueMap<V>(PhantomData<V>);

    impl<'de, V: Deserialize<'de>> de::Visitor<'de> for UniqueMap<V> {
        type Value = BTreeMap<String, V>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("an object with unique keys")
        }

        fn visit_map<A: de::MapAccess<'de>>(self, mut entries: A) -> Result<Self::Value, A::Error> {
            let mut values = BTreeMap::new();
            while let Some((key, value)) = entries.next_entry::<String, V>()? {
                match values.entry(key) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(value);
                    }
                    std::collections::btree_map::Entry::Occupied(entry) => {
                        return Err(de::Error::custom(format_args!(
                            "duplicate object key `{}`",
                            entry.key()
                        )));
                    }
                }
            }
            Ok(values)
        }
    }

    deserializer.deserialize_map(UniqueMap(PhantomData))
}

fn finite<S: Serializer>(value: f64) -> Result<f64, S::Error> {
    if !value.is_finite() {
        return Err(serde::ser::Error::custom("IR numbers must be finite"));
    }
    Ok(canonical_zero(value))
}

pub(crate) fn canonical_zero(value: f64) -> f64 {
    // Floating-point equality treats both signs of zero as equal.
    if value == 0.0 { 0.0 } else { value }
}

pub(crate) fn number<S: Serializer>(value: &f64, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_f64(finite::<S>(*value)?)
}

pub(crate) fn vec3<S: Serializer>(value: &[f64; 3], serializer: S) -> Result<S::Ok, S::Error> {
    let [x, y, z] = *value;
    [finite::<S>(x)?, finite::<S>(y)?, finite::<S>(z)?].serialize(serializer)
}
