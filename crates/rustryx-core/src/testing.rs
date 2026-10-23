//! Helpers shared by the unit tests of this crate. Compiled only for tests.

use std::fmt::Debug;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Asserts that `value` serializes to exactly the JSON text `expected` and reads back equal.
pub(crate) fn assert_wire<T>(value: &T, expected: &str)
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let json = serde_json::to_string(value).expect("serialize");
    assert_eq!(json, expected, "wire form of {value:?}");
    let back: T = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(&back, value, "round trip of {value:?}");
}

/// Serializes and reads back a value that has no `PartialEq`, returning the JSON for inspection.
pub(crate) fn round_trip_json<T>(value: &T) -> serde_json::Value
where
    T: Serialize + DeserializeOwned,
{
    let json = serde_json::to_value(value).expect("serialize");
    let back: T = serde_json::from_value(json.clone()).expect("deserialize");
    let again = serde_json::to_value(&back).expect("serialize again");
    assert_eq!(json, again, "second serialization differs");
    json
}

/// A fixed instant, so constructor tests can compare timestamps.
pub(crate) fn at() -> time::OffsetDateTime {
    time::macros::datetime!(2026-10-24 10:00:00 UTC)
}
