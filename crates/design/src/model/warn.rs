//! Warnings about values the model doesn't support, such as an unknown
//! blend mode. Reading one falls back to the default (or keeps a paint or
//! effect of an unknown type as written, undrawn) and warns, rather than
//! failing the whole read; an edit collects its warnings for the call's
//! result ([`collect`]), and reading a file ignores them.

use std::cell::RefCell;

use serde_json::{Map, Value};

thread_local! {
    static WARNINGS: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

/// Runs `read`, returning what it gives and the warnings it raised.
pub fn collect<T>(read: impl FnOnce() -> T) -> (T, Vec<String>) {
    /// Puts back the warnings of the `collect` this one runs in, even when
    /// `read` panics.
    struct Outer(Option<Vec<String>>);
    impl Drop for Outer {
        fn drop(&mut self) {
            WARNINGS.with(|w| *w.borrow_mut() = self.0.take());
        }
    }
    let _outer = Outer(WARNINGS.with(|w| w.borrow_mut().replace(Vec::new())));
    let value = read();
    let warnings = WARNINGS.with(|w| w.borrow_mut().take());
    (value, warnings.unwrap_or_default())
}

/// Notes a warning for the [`collect`] running on this thread, if any.
pub(crate) fn warn(message: impl Into<String>) {
    WARNINGS.with(|w| {
        if let Some(warnings) = w.borrow_mut().as_mut() {
            warnings.push(message.into());
        }
    });
}

/// The value named `key` among `options` (name, value), or `None` when it's
/// absent; an unknown name warns, saying what `what` it was meant to be.
pub(crate) fn choice<T: Copy>(
    props: &Map<String, Value>,
    key: &str,
    what: &str,
    options: &[(&str, T)],
) -> Result<Option<T>, String> {
    match props.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(name)) => {
            let found = options.iter().find(|(option, _)| option == name).map(|(_, v)| *v);
            if found.is_none() {
                warn(unsupported(what, name, options.iter().map(|(option, _)| *option)));
            }
            Ok(found)
        }
        Some(other) => Err(format!("a {what} is a name, not {other}")),
    }
}

/// The name `value` has among `options`.
pub(crate) fn name_of<T: PartialEq + Copy>(options: &[(&'static str, T)], value: T) -> &'static str {
    options.iter().find(|(_, v)| *v == value).map(|(name, _)| *name).unwrap_or_default()
}

/// The message for a value `name` that isn't one of `supported`.
pub(crate) fn unsupported<'a>(what: &str, name: &str, supported: impl Iterator<Item = &'a str>) -> String {
    format!(
        "unsupported {what} \"{name}\" (supported: {}); it has no effect",
        supported.collect::<Vec<_>>().join(", ")
    )
}

/// The number named `key`, if given.
pub(crate) fn number(props: &Map<String, Value>, key: &str) -> Result<Option<f32>, String> {
    match props.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => Ok(n.as_f64().map(|n| n as f32)),
        Some(other) => Err(format!("\"{key}\" is a number, not {other}")),
    }
}

/// The boolean named `key`, `default` when it's absent.
pub(crate) fn flag(props: &Map<String, Value>, key: &str, default: bool) -> Result<bool, String> {
    match props.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(other) => Err(format!("\"{key}\" is true or false, not {other}")),
    }
}

/// `{ "x": .., "y": .. }` (or `width` and `height`), each defaulting.
pub(crate) fn pair(
    props: &Map<String, Value>,
    key: &str,
    (a, b): (&str, &str),
    default: (f32, f32),
) -> Result<(f32, f32), String> {
    match props.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Object(pair)) => Ok((
            number(pair, a)?.unwrap_or(default.0),
            number(pair, b)?.unwrap_or(default.1),
        )),
        Some(other) => Err(format!("\"{key}\" is {{ \"{a}\": .., \"{b}\": .. }}, not {other}")),
    }
}

/// A number below zero, as zero, with a warning.
pub(crate) fn at_least_zero(what: &str, value: f32) -> f32 {
    if value < 0. {
        warn(format!("{what} can't be negative ({value}); 0 used"));
        0.
    } else {
        value
    }
}
