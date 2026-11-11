//! A node's CSS declarations as Tailwind classes: Tailwind's own where a
//! declaration is one exactly (`flex`, `w-[320px]`), and an arbitrary
//! property (`[backdrop-filter:blur(4px)]`) for the rest, so nothing is
//! lost on the way.

/// The classes for `declarations`, in their order.
pub fn classes(declarations: &[(String, String)]) -> String {
    let classes: Vec<String> = declarations.iter().map(|(property, value)| class(property, value)).collect();
    classes.join(" ")
}

fn class(property: &str, value: &str) -> String {
    let exact = match (property, value) {
        ("display", "flex") => Some("flex"),
        ("flex-direction", "row") => Some("flex-row"),
        ("flex-direction", "column") => Some("flex-col"),
        ("position", "relative") => Some("relative"),
        ("position", "absolute") => Some("absolute"),
        ("overflow", "hidden") => Some("overflow-hidden"),
        ("flex", "1 1 0") => Some("flex-1"),
        ("flex-shrink", "0") => Some("shrink-0"),
        ("align-self", "stretch") => Some("self-stretch"),
        ("min-width", "0") => Some("min-w-0"),
        ("min-height", "0") => Some("min-h-0"),
        ("justify-content", "flex-start") => Some("justify-start"),
        ("justify-content", "center") => Some("justify-center"),
        ("justify-content", "flex-end") => Some("justify-end"),
        ("justify-content", "space-between") => Some("justify-between"),
        ("justify-content", "space-around") => Some("justify-around"),
        ("align-items", "flex-start") => Some("items-start"),
        ("align-items", "center") => Some("items-center"),
        ("align-items", "flex-end") => Some("items-end"),
        ("white-space", "pre") => Some("whitespace-pre"),
        ("white-space", "pre-wrap") => Some("whitespace-pre-wrap"),
        ("text-align", "center") => Some("text-center"),
        ("text-align", "right") => Some("text-right"),
        ("text-align", "justify") => Some("text-justify"),
        ("text-decoration", "underline") => Some("underline"),
        ("text-decoration", "line-through") => Some("line-through"),
        ("border-radius", "50%") => Some("rounded-full"),
        _ => None,
    };
    if let Some(exact) = exact {
        return exact.to_owned();
    }
    let prefix = match property {
        "width" => Some("w"),
        "height" => Some("h"),
        "left" => Some("left"),
        "top" => Some("top"),
        "gap" => Some("gap"),
        "padding" => Some("p"),
        "border-radius" => Some("rounded"),
        "opacity" => Some("opacity"),
        "font-size" => Some("text"),
        "font-weight" => Some("font"),
        "line-height" => Some("leading"),
        "letter-spacing" => Some("tracking"),
        // A colour is told apart from a size by Tailwind's type hint.
        "background-color" => Some("bg"),
        "color" => Some("text"),
        _ => None,
    };
    match prefix {
        Some("text") if property == "color" => format!("text-[color:{}]", arbitrary(value)),
        Some("bg") => format!("bg-[color:{}]", arbitrary(value)),
        Some(prefix) => format!("{prefix}-[{}]", arbitrary(value)),
        None => format!("[{property}:{}]", arbitrary(value)),
    }
}

/// `value` as Tailwind's arbitrary values take it: spaces as underscores
/// (and underscores escaped).
fn arbitrary(value: &str) -> String {
    value.replace('_', "\\_").replace(' ', "_")
}
