//! Paint: what fills a node's area. A `fill` is one paint or a list of them
//! drawn in order, each a solid colour, a gradient or an image
//! ([`super::image`]), with a blend mode; a
//! plain solid colour is written as its hex string. A paint of a type not
//! supported is kept as written, but not drawn.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value, json};

use super::image::ImageFill;
use super::warn::{self, choice, flag, name_of, number, pair, warn};

/// The fill property group, flattened into the types that have a fill.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Fill {
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        serialize_with = "one_or_many",
        deserialize_with = "paints"
    )]
    pub fill: Vec<Paint>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Paint {
    pub kind: PaintKind,
    pub blend: BlendMode,
    /// 0 to 1, applied over the paint's own colours' alpha.
    pub opacity: f32,
    /// A disabled paint is kept but not drawn.
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PaintKind {
    Solid(Color),
    Gradient(Gradient),
    Image(ImageFill),
    /// A paint type (or gradient type) not supported, as written.
    Unsupported(Value),
}

/// A gradient over the node's bounds. Its geometry is relative to them:
/// `center` (0 to 1 across each side) and `size` (1 is the side's length).
/// A linear gradient runs `size.height` long through the center; a radial
/// or angular one fills the ellipse `size` across. `rotation` turns it,
/// in degrees counter-clockwise: at 0 a linear gradient runs upwards and
/// an angular one starts at the top and sweeps clockwise.
#[derive(Debug, Clone, PartialEq)]
pub struct Gradient {
    pub shape: GradientShape,
    pub stops: Vec<Stop>,
    pub center: (f32, f32),
    pub size: (f32, f32),
    pub rotation: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GradientShape {
    Linear,
    Radial,
    Angular,
}

const GRADIENT_SHAPES: &[(&str, GradientShape)] = &[
    ("linear", GradientShape::Linear),
    ("radial", GradientShape::Radial),
    ("angular", GradientShape::Angular),
];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stop {
    pub color: Color,
    /// 0 to 1 along the gradient.
    pub position: f32,
}

/// How a paint (or shadow) combines with what's under it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BlendMode {
    #[default]
    Normal,
    Darken,
    Multiply,
    ColorBurn,
    Lighten,
    Screen,
    ColorDodge,
    LinearDodge,
    Overlay,
    SoftLight,
    HardLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl BlendMode {
    const NAMES: &[(&str, BlendMode)] = &[
        ("normal", BlendMode::Normal),
        ("darken", BlendMode::Darken),
        ("multiply", BlendMode::Multiply),
        ("colorBurn", BlendMode::ColorBurn),
        ("lighten", BlendMode::Lighten),
        ("screen", BlendMode::Screen),
        ("colorDodge", BlendMode::ColorDodge),
        ("linearDodge", BlendMode::LinearDodge),
        ("overlay", BlendMode::Overlay),
        ("softLight", BlendMode::SoftLight),
        ("hardLight", BlendMode::HardLight),
        ("difference", BlendMode::Difference),
        ("exclusion", BlendMode::Exclusion),
        ("hue", BlendMode::Hue),
        ("saturation", BlendMode::Saturation),
        ("color", BlendMode::Color),
        ("luminosity", BlendMode::Luminosity),
    ];

    /// `blendMode` among `props`, normal when absent or unsupported.
    pub(crate) fn read(props: &Map<String, Value>) -> Result<Self, String> {
        Ok(choice(props, "blendMode", "blend mode", Self::NAMES)?.unwrap_or_default())
    }

    pub(crate) fn write(self, props: &mut Map<String, Value>) {
        if self != BlendMode::Normal {
            props.insert("blendMode".into(), json!(name_of(Self::NAMES, self)));
        }
    }
}

impl Paint {
    /// A plain solid colour.
    pub fn solid(color: Color) -> Self {
        Self {
            kind: PaintKind::Solid(color),
            blend: BlendMode::Normal,
            opacity: 1.,
            enabled: true,
        }
    }

    fn to_value(&self) -> Value {
        let mut props = Map::new();
        match &self.kind {
            PaintKind::Unsupported(value) => return value.clone(),
            PaintKind::Solid(color) => {
                if self.blend == BlendMode::Normal && self.opacity == 1. && self.enabled {
                    return json!(color.to_hex());
                }
                props.insert("type".into(), json!("color"));
                props.insert("color".into(), json!(color.to_hex()));
            }
            PaintKind::Gradient(gradient) => {
                props.insert("type".into(), json!("gradient"));
                props.insert("gradientType".into(), json!(name_of(GRADIENT_SHAPES, gradient.shape)));
                if gradient.center != (0.5, 0.5) {
                    props.insert("center".into(), json!({ "x": gradient.center.0, "y": gradient.center.1 }));
                }
                if gradient.size != (1., 1.) {
                    props.insert("size".into(), json!({ "width": gradient.size.0, "height": gradient.size.1 }));
                }
                if gradient.rotation != 0. {
                    props.insert("rotation".into(), json!(gradient.rotation));
                }
                let stops: Vec<_> = gradient
                    .stops
                    .iter()
                    .map(|stop| json!({ "color": stop.color.to_hex(), "position": stop.position }))
                    .collect();
                props.insert("colors".into(), Value::Array(stops));
            }
            PaintKind::Image(image) => image.write(&mut props),
        }
        if self.opacity != 1. {
            props.insert("opacity".into(), json!(self.opacity));
        }
        if !self.enabled {
            props.insert("enabled".into(), json!(false));
        }
        self.blend.write(&mut props);
        Value::Object(props)
    }

    /// The paint `value` describes; one of a type that isn't supported is
    /// kept as written, with a warning.
    fn from_value(value: &Value) -> Result<Self, String> {
        let paint = match value {
            Value::String(hex) => return Color::parse(hex).map(Paint::solid),
            Value::Object(paint) => paint,
            _ => return Err(format!("a paint is a hex colour or a paint object, not {value}")),
        };
        let kind = match paint.get("type").and_then(Value::as_str) {
            Some("color") => match paint.get("color") {
                Some(Value::String(hex)) => PaintKind::Solid(Color::parse(hex)?),
                _ => return Err("a color paint needs a \"color\" hex string".into()),
            },
            Some("gradient") => match Gradient::from_props(paint)? {
                Some(gradient) => PaintKind::Gradient(gradient),
                None => PaintKind::Unsupported(value.clone()),
            },
            Some("image") => PaintKind::Image(ImageFill::from_props(paint)?),
            Some(other) => {
                warn(warn::unsupported("paint type", other, ["color", "gradient", "image"].into_iter()));
                PaintKind::Unsupported(value.clone())
            }
            None => return Err("a paint needs a \"type\"".into()),
        };
        if let PaintKind::Unsupported(_) = kind {
            return Ok(Paint { kind, blend: BlendMode::Normal, opacity: 1., enabled: true });
        }
        Ok(Paint {
            kind,
            blend: BlendMode::read(paint)?,
            opacity: opacity(paint)?,
            enabled: flag(paint, "enabled", true)?,
        })
    }
}

/// `opacity` among `props`, 1 when absent, kept within 0 to 1.
pub(crate) fn opacity(props: &Map<String, Value>) -> Result<f32, String> {
    let opacity = number(props, "opacity")?.unwrap_or(1.);
    if !(0. ..=1.).contains(&opacity) {
        warn(format!("opacity goes from 0 to 1, not {opacity}; clamped"));
    }
    Ok(opacity.clamp(0., 1.))
}

impl Gradient {
    fn from_props(paint: &Map<String, Value>) -> Result<Option<Self>, String> {
        let shape = match paint.get("gradientType") {
            None => GradientShape::Linear,
            Some(_) => match choice(paint, "gradientType", "gradient type", GRADIENT_SHAPES)? {
                Some(shape) => shape,
                None => return Ok(None),
            },
        };
        let mut stops = match paint.get("colors") {
            Some(Value::Array(stops)) => stops
                .iter()
                .enumerate()
                .map(|(i, stop)| Stop::from_value(stop, i, stops.len()))
                .collect::<Result<Vec<_>, _>>()?,
            _ => return Err("a gradient needs \"colors\": a list of { color, position }".into()),
        };
        if stops.len() < 2 {
            return Err("a gradient needs at least two colors".into());
        }
        stops.sort_by(|a, b| a.position.total_cmp(&b.position));
        Ok(Some(Self {
            shape,
            stops,
            center: pair(paint, "center", ("x", "y"), (0.5, 0.5))?,
            size: pair(paint, "size", ("width", "height"), (1., 1.))?,
            rotation: number(paint, "rotation")?.unwrap_or(0.),
        }))
    }
}

impl Stop {
    /// The `index`th of `count` stops: a hex string spaced evenly, or
    /// `{ color, position }`.
    fn from_value(value: &Value, index: usize, count: usize) -> Result<Self, String> {
        let even = index as f32 / (count.max(2) - 1) as f32;
        match value {
            Value::String(hex) => Ok(Self { color: Color::parse(hex)?, position: even }),
            Value::Object(stop) => {
                let color = match stop.get("color") {
                    Some(Value::String(hex)) => Color::parse(hex)?,
                    _ => return Err("a gradient color needs a \"color\" hex string".into()),
                };
                let position = number(stop, "position")?.unwrap_or(even);
                if !(0. ..=1.).contains(&position) {
                    warn(format!("a gradient position goes from 0 to 1, not {position}; clamped"));
                }
                Ok(Self { color, position: position.clamp(0., 1.) })
            }
            _ => Err(format!("a gradient color is {{ color, position }}, not {value}")),
        }
    }
}

fn one_or_many<S: Serializer>(paints: &[Paint], serializer: S) -> Result<S::Ok, S::Error> {
    paints_to_value(paints).serialize(serializer)
}

fn paints<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Paint>, D::Error> {
    paints_from(&Value::deserialize(deserializer)?).map_err(D::Error::custom)
}

/// One paint or a list of them.
pub(crate) fn paints_from(value: &Value) -> Result<Vec<Paint>, String> {
    match value {
        Value::Array(paints) => paints.iter().map(Paint::from_value).collect(),
        Value::Null => Ok(Vec::new()),
        paint => Ok(vec![Paint::from_value(paint)?]),
    }
}

pub(crate) fn paints_to_value(paints: &[Paint]) -> Value {
    match paints {
        [paint] => paint.to_value(),
        paints => Value::Array(paints.iter().map(Paint::to_value).collect()),
    }
}

/// An sRGB colour with alpha, written `#RRGGBB` or `#RRGGBBAA`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    /// `#RGB`, `#RRGGBB` or `#RRGGBBAA`.
    pub fn parse(hex: &str) -> Result<Self, String> {
        let invalid = || format!("\"{hex}\" is not a colour (#RGB, #RRGGBB or #RRGGBBAA)");
        let digits = hex.strip_prefix('#').ok_or_else(invalid)?;
        if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(invalid());
        }
        let byte = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).map_err(|_| invalid());
        match digits.len() {
            3 => {
                let nibble = |at: usize| {
                    u8::from_str_radix(&digits[at..at + 1], 16)
                        .map(|n| n * 17)
                        .map_err(|_| invalid())
                };
                Ok(Self { r: nibble(0)?, g: nibble(1)?, b: nibble(2)?, a: 255 })
            }
            6 => Ok(Self { r: byte(0)?, g: byte(2)?, b: byte(4)?, a: 255 }),
            8 => Ok(Self { r: byte(0)?, g: byte(2)?, b: byte(4)?, a: byte(6)? }),
            _ => Err(invalid()),
        }
    }

    pub fn to_hex(self) -> String {
        let Self { r, g, b, a } = self;
        if a == 255 {
            format!("#{r:02X}{g:02X}{b:02X}")
        } else {
            format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
        }
    }
}
