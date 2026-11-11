//! Corner radii: one for every corner, or four (top left, top right,
//! bottom right, bottom left), as `cornerRadius`.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Value, json};

use super::warn::at_least_zero;

/// The corner radius property group, flattened into the types that have it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Corners {
    #[serde(
        default,
        rename = "cornerRadius",
        skip_serializing_if = "is_square",
        serialize_with = "write",
        deserialize_with = "read"
    )]
    pub radii: [f32; 4],
}

fn is_square(radii: &[f32; 4]) -> bool {
    radii.iter().all(|r| *r == 0.)
}

fn write<S: Serializer>(radii: &[f32; 4], serializer: S) -> Result<S::Ok, S::Error> {
    if radii.iter().all(|r| *r == radii[0]) {
        json!(radii[0]).serialize(serializer)
    } else {
        radii.serialize(serializer)
    }
}

fn read<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[f32; 4], D::Error> {
    let radius = |value: &Value| {
        value
            .as_f64()
            .map(|r| at_least_zero("a corner radius", r as f32))
            .ok_or_else(|| D::Error::custom(format!("a corner radius is a number, not {value}")))
    };
    match Value::deserialize(deserializer)? {
        Value::Null => Ok([0.; 4]),
        Value::Array(radii) if radii.len() == 4 => {
            Ok([radius(&radii[0])?, radius(&radii[1])?, radius(&radii[2])?, radius(&radii[3])?])
        }
        Value::Array(_) => Err(D::Error::custom(
            "\"cornerRadius\" is one number or four (top left, top right, bottom right, bottom left)",
        )),
        value => Ok([radius(&value)?; 4]),
    }
}
