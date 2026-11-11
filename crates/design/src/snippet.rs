//! The snippet sandbox: an agent's JavaScript, run by QuickJS against a
//! [`Draft`] with limits on its memory, stack and time.
//!
//! The snippet sees only the API installed here (no files, network or
//! modules). Each function is a native one under `__zd` wrapped by the
//! prelude, which turns its arguments into JSON; adding one is a native in
//! [`install`] and a line in [`PRELUDE`].
//!
//! Each call runs in a fresh context, but for [`Globals`]: what a snippet
//! assigns without declaring it is carried to the chat's next call.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rquickjs::context::EvalOptions;
use rquickjs::{CatchResultExt, CaughtError, Context, Ctx, Exception, Function, Object, Runtime};

use crate::edit::Draft;
use crate::layout::LaidOut;

mod globals;

pub use globals::Globals;

/// What one snippet may use.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub time: Duration,
    /// QuickJS's heap, in bytes.
    pub memory: usize,
    /// QuickJS's share of its thread's stack, in bytes.
    pub stack: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            time: Duration::from_secs(5),
            memory: 64 * 1024 * 1024,
            stack: 1024 * 1024,
        }
    }
}

/// Room on the snippet's own thread past QuickJS's limit, for the natives
/// it calls and Rust's own frames.
const THREAD_STACK_HEADROOM: usize = 2 * 1024 * 1024;

/// The name the snippet runs as, for finding its lines in a stack.
const SNIPPET_FILE: &str = "snippet";

#[derive(Debug, thiserror::Error)]
pub enum SnippetError {
    /// The snippet threw, or broke a limit QuickJS reports as an error
    /// (memory, stack); `line` is the snippet's line it failed on, from 1,
    /// when QuickJS says.
    #[error("{}{message}", line.map(|line| format!("line {line}: ")).unwrap_or_default())]
    Failed { message: String, line: Option<usize> },
    #[error("the snippet ran past its {}-second limit", .0.as_secs_f32())]
    TimedOut(Duration),
    #[error("the snippet sandbox failed: {0}")]
    Sandbox(String),
}

/// The API, as the snippet calls it.
const PRELUDE: &str = r#"(function (native) {
  const json = (value) => JSON.stringify(value === undefined ? null : value);
  globalThis.Insert = function (parent, node) {
    if (arguments.length < 2) { node = parent; parent = null; }
    return native.insert(parent == null ? null : String(parent), json(node));
  };
  const id = (value) => value == null ? null : String(value);
  globalThis.Copy = function (node, parent, props) {
    return native.copy(String(node), id(parent), json(props));
  };
  globalThis.Update = function (node, props) {
    native.update(String(node), json(props));
  };
  globalThis.Replace = function (node, replacement) {
    return native.replace(String(node), json(replacement));
  };
  globalThis.Move = function (node, parent, index) {
    native.move(String(node), id(parent), index == null ? null : Number(index));
  };
  globalThis.Delete = function (nodes) {
    native.delete(json((Array.isArray(nodes) ? nodes : [nodes]).map(String)));
  };
  globalThis.SetTokens = function (tokens, options) {
    native.setTokens(json(tokens), Boolean(options && options.replace));
  };
  globalThis.GetTokens = function () {
    return JSON.parse(native.getTokens());
  };
  globalThis.FindEmptySpace = function (wanted) {
    return JSON.parse(native.emptySpace(json(wanted)));
  };
  globalThis.TakeScreenshot = function (ids) {
    native.screenshot(json(Array.isArray(ids) ? ids : [ids]));
  };
  globalThis.Get = function (id, options, visiting) {
    if (typeof id === "function" || (id !== null && typeof id === "object")) { visiting = options; options = id; id = null; }
    id = id == null ? null : String(id);
    if (typeof options === "function") {
      let skipping = Infinity;
      for (const { node, info } of JSON.parse(native.visit(id, json(visiting || {})))) {
        if (info.depth > skipping) continue;
        skipping = options(node, info) === false ? info.depth : Infinity;
      }
      return;
    }
    const depth = options == null || options.depth === undefined ? 1 : Number(options.depth);
    return JSON.parse(native.get(id, depth, json(options || {})));
  };
  globalThis.Print = function (...values) {
    native.print(values.map((value) => typeof value === "string" ? value : String(JSON.stringify(value) ?? value)).join(" "));
  };
})(globalThis.__zd);
delete globalThis.__zd;
"#;

/// Runs `code` against `draft` with a chat's `globals` on a thread of its
/// own, giving the draft back with the snippet's edits, and the globals it
/// left, when it ran to its end.
pub fn run(
    draft: Draft,
    globals: Globals,
    code: &str,
    limits: Limits,
) -> Result<(Draft, Globals), SnippetError> {
    let code = code.to_owned();
    std::thread::Builder::new()
        .name("design-snippet".into())
        .stack_size(limits.stack + THREAD_STACK_HEADROOM)
        .spawn(move || run_here(draft, &globals, &code, limits))
        .map_err(|e| SnippetError::Sandbox(e.to_string()))?
        .join()
        .map_err(|_| SnippetError::Sandbox("the snippet's thread panicked".into()))?
}

fn run_here(
    draft: Draft,
    globals: &Globals,
    code: &str,
    limits: Limits,
) -> Result<(Draft, Globals), SnippetError> {
    let sandbox = |e: rquickjs::Error| SnippetError::Sandbox(e.to_string());
    let runtime = Runtime::new().map_err(sandbox)?;
    runtime.set_memory_limit(limits.memory);
    runtime.set_max_stack_size(limits.stack);
    let timed_out = Arc::new(AtomicBool::new(false));
    {
        let (deadline, timed_out) = (Instant::now() + limits.time, timed_out.clone());
        runtime.set_interrupt_handler(Some(Box::new(move || {
            let past = Instant::now() >= deadline;
            if past {
                timed_out.store(true, Ordering::Relaxed);
            }
            past
        })));
    }
    let context = Context::full(&runtime).map_err(sandbox)?;
    let draft = Rc::new(RefCell::new(draft));
    let ran = context.with(|ctx| {
        install(&ctx, &draft).map_err(sandbox)?;
        ctx.eval::<(), _>(PRELUDE).map_err(sandbox)?;
        let builtins = globals::names(&ctx).map_err(sandbox)?;
        globals::restore(&ctx, globals).map_err(sandbox)?;
        // On the snippet's first line, so its errors' line numbers are its own.
        let wrapped = format!("(function () {{{code}\n}})();");
        // Not strict, so assigning an undeclared name makes a global.
        let mut options = EvalOptions::default();
        options.strict = false;
        options.filename = Some(SNIPPET_FILE.into());
        ctx.eval_with_options::<(), _>(wrapped, options)
            .catch(&ctx)
            .map_err(failed)?;
        globals::assigned(&ctx, &builtins).catch(&ctx).map_err(failed)
    });
    if timed_out.load(Ordering::Relaxed) {
        return Err(SnippetError::TimedOut(limits.time));
    }
    let globals = ran?;
    drop(context);
    drop(runtime);
    let draft = Rc::try_unwrap(draft)
        .map(RefCell::into_inner)
        .map_err(|_| SnippetError::Sandbox("the snippet's draft is still held".into()))?;
    Ok((draft, globals))
}

/// The natives under `__zd`, for [`PRELUDE`] to wrap.
fn install<'js>(ctx: &Ctx<'js>, draft: &Rc<RefCell<Draft>>) -> rquickjs::Result<()> {
    let native = Object::new(ctx.clone())?;
    {
        let draft = draft.clone();
        native.set(
            "insert",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, parent: Option<String>, node: String| {
                let node = serde_json::from_str(&node).map_err(|e| thrown(&ctx, e))?;
                draft
                    .borrow_mut()
                    .insert(parent.as_deref(), node)
                    .map_err(|e| thrown(&ctx, e))
            })?,
        )?;
    }
    {
        let draft = draft.clone();
        native.set(
            "copy",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, id: String, parent: Option<String>, props: String| {
                    let props = serde_json::from_str(&props).map_err(|e| thrown(&ctx, e))?;
                    draft
                        .borrow_mut()
                        .copy(&id, parent.as_deref(), props)
                        .map_err(|e| thrown(&ctx, e))
                },
            )?,
        )?;
    }
    {
        let draft = draft.clone();
        native.set(
            "update",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, id: String, props: String| {
                let props = serde_json::from_str(&props).map_err(|e| thrown(&ctx, e))?;
                draft.borrow_mut().update(&id, props).map_err(|e| thrown(&ctx, e))
            })?,
        )?;
    }
    {
        let draft = draft.clone();
        native.set(
            "replace",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, id: String, node: String| {
                let node = serde_json::from_str(&node).map_err(|e| thrown(&ctx, e))?;
                draft.borrow_mut().replace(&id, node).map_err(|e| thrown(&ctx, e))
            })?,
        )?;
    }
    {
        let draft = draft.clone();
        native.set(
            "move",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, id: String, parent: Option<String>, index: Option<f64>| {
                    let index = match index {
                        Some(index) if index >= 0. && index.fract() == 0. => Some(index as usize),
                        Some(index) => {
                            return Err(thrown(&ctx, format!("Move's index is a whole number, 0 or more, not {index}")));
                        }
                        None => None,
                    };
                    draft
                        .borrow_mut()
                        .move_to(&id, parent.as_deref(), index)
                        .map_err(|e| thrown(&ctx, e))
                },
            )?,
        )?;
    }
    {
        let draft = draft.clone();
        native.set(
            "delete",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, ids: String| {
                let ids: Vec<String> = serde_json::from_str(&ids).map_err(|e| thrown(&ctx, e))?;
                draft.borrow_mut().delete(&ids).map_err(|e| thrown(&ctx, e))
            })?,
        )?;
    }
    {
        let draft = draft.clone();
        native.set(
            "setTokens",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, tokens: String, replace: bool| {
                let tokens = serde_json::from_str(&tokens).map_err(|e| thrown(&ctx, e))?;
                draft.borrow_mut().set_tokens(tokens, replace).map_err(|e| thrown(&ctx, e))
            })?,
        )?;
    }
    {
        let draft = draft.clone();
        native.set(
            "getTokens",
            Function::new(ctx.clone(), move || draft.borrow().tokens().to_string())?,
        )?;
    }
    {
        let draft = draft.clone();
        native.set(
            "emptySpace",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, wanted: String| {
                let wanted: crate::space::Wanted = serde_json::from_str(&wanted).map_err(|e| {
                    thrown(&ctx, format!("FindEmptySpace takes {{ width, height, direction, padding, from }}: {e}"))
                })?;
                let draft = draft.borrow();
                let drawn = crate::resolve::drawn(draft.design());
                let typesetter = draft.fonts().typesetter(&drawn);
                let laid_out = LaidOut::of(&drawn, &typesetter);
                let (x, y) = crate::space::find(&drawn, &laid_out, &wanted).map_err(|e| thrown(&ctx, e))?;
                Ok::<_, rquickjs::Error>(serde_json::json!({ "x": x, "y": y }).to_string())
            })?,
        )?;
    }
    {
        let draft = draft.clone();
        native.set(
            "screenshot",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, ids: String| {
                let ids: Vec<String> = serde_json::from_str(&ids)
                    .map_err(|_| thrown(&ctx, "TakeScreenshot takes a list of node ids"))?;
                draft.borrow_mut().screenshot(ids).map_err(|e| thrown(&ctx, e))
            })?,
        )?;
    }
    {
        let draft = draft.clone();
        native.set(
            "get",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, id: Option<String>, depth: f64, options: String| {
                    let depth = crate::read::depth(depth).map_err(|e| thrown(&ctx, e))?;
                    let options = read_options(&ctx, &options)?;
                    let draft = draft.borrow();
                    let view = crate::read::view(draft.design(), id.as_deref(), &options);
                    crate::read::nodes(&view, id.as_deref(), depth)
                        .map(|nodes| nodes.to_string())
                        .map_err(|e| thrown(&ctx, e))
                },
            )?,
        )?;
    }
    {
        let draft = draft.clone();
        native.set(
            "visit",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, id: Option<String>, options: String| {
                let options = read_options(&ctx, &options)?;
                let draft = draft.borrow();
                let view = crate::read::view(draft.design(), id.as_deref(), &options);
                // Where nodes are is where they're drawn.
                let drawn = crate::resolve::drawn(draft.design());
                let typesetter = draft.fonts().typesetter(&drawn);
                let laid_out = LaidOut::of(&drawn, &typesetter);
                crate::read::visit(&view, &laid_out, id.as_deref())
                    .map(|visited| serde_json::Value::Array(visited).to_string())
                    .map_err(|e| thrown(&ctx, e))
            })?,
        )?;
    }
    {
        let draft = draft.clone();
        native.set(
            "print",
            Function::new(ctx.clone(), move |line: String| draft.borrow_mut().print(line))?,
        )?;
    }
    ctx.globals().set("__zd", native)
}

/// What the snippet failed with, and on which of its lines.
fn failed(caught: CaughtError<'_>) -> SnippetError {
    let (message, stack) = match &caught {
        CaughtError::Exception(exception) => {
            let name: Option<String> = exception.get("name").ok();
            let message = exception.message().unwrap_or_default();
            let message = match name {
                Some(name) if !name.is_empty() => format!("{name}: {message}"),
                _ => message,
            };
            (message, exception.stack())
        }
        other => (other.to_string(), None),
    };
    SnippetError::Failed {
        message,
        line: stack.as_deref().and_then(snippet_line),
    }
}

/// The snippet's innermost line in a QuickJS stack (`at f (snippet:3:7)`).
fn snippet_line(stack: &str) -> Option<usize> {
    let marker = format!("{SNIPPET_FILE}:");
    stack.match_indices(&marker).find_map(|(at, _)| {
        let digits: String = stack[at + marker.len()..].chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    })
}

/// `Get`'s options, as the prelude passes them.
fn read_options(ctx: &Ctx<'_>, options: &str) -> rquickjs::Result<crate::read::Options> {
    serde_json::from_str(options).map_err(|e| thrown(ctx, format!("Get's options: {e}")))
}

fn thrown(ctx: &Ctx<'_>, message: impl std::fmt::Display) -> rquickjs::Error {
    Exception::throw_message(ctx, &message.to_string())
}
