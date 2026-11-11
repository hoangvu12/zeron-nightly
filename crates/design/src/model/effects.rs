//! Effects: drop and inner shadows, a blur of the node itself (layer blur)
//! and a blur of what's behind it (background blur). An `effect` is one
//! effect or a list of them; one of a type not supported is kept as
//! written, but not drawn.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value, json};

use super::paint::BlendMode;
use super::warn::{self, at_least_zero, flag, number, pair, warn};
use super::Color;

/// The effects property group, flattened into the types that have effects.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Effects {
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        serialize_with = "write",
        deserialize_with = "read"
    )]
    pub effect: Vec<Effect>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Effect {
    pub kind: EffectKind,
    /// A disabled effect is kept but not drawn.
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EffectKind {
    Shadow(Shadow),
    /// Blurs the node and its children, by this radius.
    LayerBlur(f32),
    /// Blurs what's behind the node, within its outline, by this radius.
    BackgroundBlur(f32),
    /// An effect type not supported, as written.
    Unsupported(Value),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shadow {
    /// Inside the outline rather than behind the node.
    pub inner: bool,
    pub offset: (f32, f32),
    /// The blur's radius.
    pub blur: f32,
    /// How far the shadow's outline grows past the node's (or into it, for
    /// an inner shadow).
    pub spread: f32,
    pub color: Color,
    pub blend: BlendMode,
}

impl Effects {
    /// The enabled effects' total layer blur.
    pub fn layer_blur(&self) -> f32 {
        self.enabled()
            .filter_map(|effect| match effect {
                EffectKind::LayerBlur(radius) => Some(*radius),
                _ => None,
            })
            .sum()
    }

    pub fn enabled(&self) -> impl Iterator<Item = &EffectKind> {
        self.effect.iter().filter(|effect| effect.enabled).map(|effect| &effect.kind)
    }
}

impl Effect {
    fn to_value(&self) -> Value {
        let mut props = Map::new();
        match &self.kind {
            EffectKind::Unsupported(value) => return value.clone(),
            EffectKind::Shadow(shadow) => {
                props.insert("type".into(), json!("shadow"));
                props.insert("shadowType".into(), json!(if shadow.inner { "inner" } else { "outer" }));
                props.insert("offset".into(), json!({ "x": shadow.offset.0, "y": shadow.offset.1 }));
                props.insert("blur".into(), json!(shadow.blur));
                if shadow.spread != 0. {
                    props.insert("spread".into(), json!(shadow.spread));
                }
                props.insert("color".into(), json!(shadow.color.to_hex()));
                shadow.blend.write(&mut props);
            }
            EffectKind::LayerBlur(radius) => {
                props.insert("type".into(), json!("blur"));
                props.insert("radius".into(), json!(radius));
            }
            EffectKind::BackgroundBlur(radius) => {
                props.insert("type".into(), json!("background_blur"));
                props.insert("radius".into(), json!(radius));
            }
        }
        if !self.enabled {
            props.insert("enabled".into(), json!(false));
        }
        Value::Object(props)
    }

    /// The effect `value` describes; one of a type that isn't supported is
    /// kept as written, with a warning.
    fn from_value(value: &Value) -> Result<Self, String> {
        let Value::Object(props) = value else {
            return Err(format!("an effect is an object with a \"type\", not {value}"));
        };
        let radius = || -> Result<f32, String> {
            Ok(at_least_zero("a blur's radius", number(props, "radius")?.unwrap_or(0.)))
        };
        let kind = match props.get("type").and_then(Value::as_str) {
            Some("shadow") => EffectKind::Shadow(Shadow::from_props(props)?),
            Some("blur") => EffectKind::LayerBlur(radius()?),
            Some("background_blur") => EffectKind::BackgroundBlur(radius()?),
            Some(other) => {
                let supported = ["shadow", "blur", "background_blur"].into_iter();
                warn(warn::unsupported("effect type", other, supported));
                EffectKind::Unsupported(value.clone())
            }
            None => return Err("an effect needs a \"type\"".into()),
        };
        Ok(Self { kind, enabled: flag(props, "enabled", true)? })
    }
}

impl Shadow {
    fn from_props(props: &Map<String, Value>) -> Result<Self, String> {
        let inner = match props.get("shadowType").and_then(Value::as_str) {
            None | Some("outer") => false,
            Some("inner") => true,
            Some(other) => {
                warn(warn::unsupported("shadow type", other, ["outer", "inner"].into_iter()));
                false
            }
        };
        let color = match props.get("color") {
            None => Color { r: 0, g: 0, b: 0, a: 64 },
            Some(Value::String(hex)) => Color::parse(hex)?,
            Some(other) => return Err(format!("a shadow's \"color\" is a hex string, not {other}")),
        };
        Ok(Self {
            inner,
            offset: pair(props, "offset", ("x", "y"), (0., 0.))?,
            blur: at_least_zero("a shadow's blur", number(props, "blur")?.unwrap_or(0.)),
            spread: number(props, "spread")?.unwrap_or(0.),
            color,
            blend: BlendMode::read(props)?,
        })
    }
}

fn write<S: Serializer>(effects: &[Effect], serializer: S) -> Result<S::Ok, S::Error> {
    match effects {
        [effect] => effect.to_value().serialize(serializer),
        effects => effects.iter().map(Effect::to_value).collect::<Vec<_>>().serialize(serializer),
    }
}

fn read<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Effect>, D::Error> {
    let effects = match Value::deserialize(deserializer)? {
        Value::Array(effects) => effects,
        Value::Null => Vec::new(),
        effect => vec![effect],
    };
    effects
        .iter()
        .map(Effect::from_value)
        .collect::<Result<_, _>>()
        .map_err(D::Error::custom)
}
