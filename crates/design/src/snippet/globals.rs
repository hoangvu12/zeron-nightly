//! A chat's globals: what its snippets assign without declaring, carried
//! from one call to its next as JSON. Functions and values JSON can't hold
//! (a cycle, say) don't carry over.

use std::collections::{BTreeMap, HashSet};

use rquickjs::{Ctx, Exception};

/// A chat's globals by name, as JSON.
pub type Globals = BTreeMap<String, String>;

/// How much a chat's globals hold, as JSON, in bytes.
pub const MAX_GLOBALS: usize = 1024 * 1024;

/// The names on `globalThis` now: before a snippet runs, the API's own.
pub fn names(ctx: &Ctx<'_>) -> rquickjs::Result<HashSet<String>> {
    Ok(ctx
        .eval::<Vec<String>, _>("Object.getOwnPropertyNames(globalThis)")?
        .into_iter()
        .collect())
}

/// Sets the chat's globals before its snippet runs.
pub fn restore(ctx: &Ctx<'_>, globals: &Globals) -> rquickjs::Result<()> {
    let global = ctx.globals();
    for (name, json) in globals {
        global.set(name.as_str(), ctx.json_parse(json.as_str())?)?;
    }
    Ok(())
}

/// The globals the snippet assigned beyond the API's `builtins`; throws
/// when they hold more than [`MAX_GLOBALS`].
pub fn assigned(ctx: &Ctx<'_>, builtins: &HashSet<String>) -> rquickjs::Result<Globals> {
    let global = ctx.globals();
    let mut assigned = Globals::new();
    let mut bytes = 0;
    for name in names(ctx)? {
        if builtins.contains(&name) {
            continue;
        }
        let value: rquickjs::Value = global.get(name.as_str())?;
        match ctx.json_stringify(value) {
            Ok(Some(json)) => {
                let json = json.to_string()?;
                bytes += name.len() + json.len();
                assigned.insert(name, json);
            }
            // A function or `undefined`.
            Ok(None) => {}
            // A cycle, say: it can't carry over, and the call still stands.
            Err(rquickjs::Error::Exception) => drop(ctx.catch()),
            Err(e) => return Err(e),
        }
    }
    if bytes > MAX_GLOBALS {
        return Err(Exception::throw_message(
            ctx,
            &format!("globals carried to the next call hold at most {MAX_GLOBALS} bytes as JSON; declare big values with let or const"),
        ));
    }
    Ok(assigned)
}
