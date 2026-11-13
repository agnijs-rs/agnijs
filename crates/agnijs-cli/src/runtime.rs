//! One engine as the command drives it: the prelude (timers, the value
//! inspector, console formatting), running scripts, modules and bytecode,
//! the event loop, and turning failures into messages and exit codes.

use crate::host::{self, CliHost};
use agnijs::api::{self, CallContext, Engine, PromiseState, PropertyKey, RealmId, Value};
use agnijs::Context;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

const PRELUDE: &str = include_str!("prelude.js");
const PRELUDE_NAME: &str = "agnijs:prelude";

/// Exit statuses (documented in the crate README and `--help`).
pub mod exit {
    /// An uncaught exception or unhandled promise rejection.
    pub const UNCAUGHT: i32 = 1;
    /// Bad usage, or a file that cannot be read or written.
    pub const USAGE: i32 = 2;
    /// The fuel budget or the memory limit was exhausted.
    pub const LIMIT: i32 = 3;
    /// The main module's top-level await never settled (as Node).
    pub const UNSETTLED: i32 = 13;
    /// Interrupted with Ctrl-C (128 + SIGINT).
    pub const INTERRUPTED: i32 = 130;
}

/// Why a run stopped early. The message has been printed already.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failure(pub i32);

/// Engine limits and options from the command line.
#[derive(Clone, Debug, Default)]
pub struct Limits {
    pub fuel: Option<u64>,
    pub max_memory: Option<usize>,
}

struct Timer {
    callback: Value,
    args: Vec<Value>,
    repeat: Option<Duration>,
}

#[derive(Default)]
struct Timers {
    next_id: u32,
    /// (due, sequence) -> id: ordered by due time, then creation.
    queue: BTreeMap<(Instant, u64), u32>,
    seq: u64,
    live: std::collections::HashMap<u32, Timer>,
}

impl Timers {
    fn schedule(&mut self, id: u32, after: Duration) {
        self.seq += 1;
        self.queue.insert((Instant::now() + after, self.seq), id);
    }
}

pub struct Runtime {
    pub ctx: Context,
    pub host: Rc<CliHost>,
    timers: Rc<RefCell<Timers>>,
    inspect: Value,
    limits: Limits,
    /// The promise of the main module's evaluation, reported as its result
    /// and not as an unhandled rejection.
    pub main_promise: Option<Value>,
    /// The REPL prints every uncaught value as `Uncaught ...`, without a
    /// code frame.
    pub repl_mode: bool,
    /// The main file's name and source, for the code frame of an error in
    /// code the engine has no source for: a script that did not compile,
    /// or `[eval]` and `[stdin]`, which are not files.
    main_source: Option<(String, String)>,
}

/// A value the REPL or `eval --print` shows.
pub enum Shown {
    Value(Value),
    Nothing,
}

impl Runtime {
    pub fn new(
        ctx: Context,
        host: Rc<CliHost>,
        limits: Limits,
        patch_console: bool,
    ) -> Result<Runtime, Failure> {
        let mut rt = Runtime {
            ctx,
            host,
            timers: Rc::new(RefCell::new(Timers::default())),
            inspect: Value::Undefined,
            limits,
            main_promise: None,
            repl_mode: false,
            main_source: None,
        };
        rt.install(patch_console).map_err(|e| {
            eprintln!("agnijs: cannot start the engine: {e}");
            Failure(exit::USAGE)
        })?;
        if let Some(bytes) = rt.limits.max_memory {
            rt.ctx.engine().set_memory_limit(Some(bytes));
        }
        if let Some(fuel) = rt.limits.fuel {
            if let Err(e) = rt.ctx.engine().set_fuel(Some(fuel)) {
                eprintln!("agnijs: --fuel: {e}");
                return Err(Failure(exit::USAGE));
            }
        }
        Ok(rt)
    }

    pub fn realm(&self) -> RealmId {
        self.ctx.realm()
    }

    fn install(&mut self, patch_console: bool) -> api::Result<()> {
        let realm = self.realm();
        let timers = self.timers.clone();
        let engine = self.ctx.engine();
        let installer = engine.eval_script(realm, PRELUDE_NAME, PRELUDE)?;

        let promise_state = engine.new_function(
            realm,
            "promiseState",
            Rc::new(|engine: &mut dyn Engine, cx: CallContext| {
                let p = cx.arg(0);
                let items = match engine.promise_state(cx.realm, &p)? {
                    PromiseState::Pending => vec![Value::string("pending")],
                    PromiseState::Fulfilled(v) => vec![Value::string("fulfilled"), v],
                    PromiseState::Rejected(r) => vec![Value::string("rejected"), r],
                };
                engine.new_array(cx.realm, &items)
            }),
        )?;
        let t = timers.clone();
        let set_timer = engine.new_function(
            realm,
            "setTimer",
            Rc::new(move |engine: &mut dyn Engine, cx: CallContext| {
                let ms = engine.to_number(cx.realm, &cx.arg(1))?;
                let ms = if ms.is_finite() && ms > 0.0 {
                    ms.min(2_147_483_647.0)
                } else {
                    0.0
                };
                let after = Duration::from_micros((ms * 1000.0) as u64);
                let repeat = matches!(cx.arg(2), Value::Bool(true));
                let list = cx.arg(3);
                let len_v = engine.get(cx.realm, &list, &PropertyKey::from("length"))?;
                let len = engine.to_number(cx.realm, &len_v)? as u32;
                // The prelude passes a dense rest array, but its `length`
                // does not size the reservation (TASK-103.39's bug class).
                let mut args = Vec::with_capacity((len as usize).min(64));
                for i in 0..len {
                    args.push(engine.get(
                        cx.realm,
                        &list,
                        &PropertyKey::from(i.to_string().as_str()),
                    )?);
                }
                let mut t = t.borrow_mut();
                t.next_id += 1;
                let id = t.next_id;
                t.live.insert(
                    id,
                    Timer {
                        callback: cx.arg(0),
                        args,
                        // Node clamps a repeating timer to at least 1 ms.
                        repeat: repeat.then_some(after.max(Duration::from_millis(1))),
                    },
                );
                t.schedule(id, after);
                Ok(Value::Number(id as f64))
            }),
        )?;
        let t = timers;
        let clear_timer = engine.new_function(
            realm,
            "clearTimer",
            Rc::new(move |engine: &mut dyn Engine, cx: CallContext| {
                let id = engine.to_number(cx.realm, &cx.arg(0))?;
                if id.is_finite() && id >= 0.0 {
                    t.borrow_mut().live.remove(&(id as u32));
                }
                Ok(Value::Undefined)
            }),
        )?;
        let host = engine.new_object(realm)?;
        engine.set(realm, &host, &"promiseState".into(), &promise_state)?;
        engine.set(realm, &host, &"setTimer".into(), &set_timer)?;
        engine.set(realm, &host, &"clearTimer".into(), &clear_timer)?;
        engine.set(
            realm,
            &host,
            &"patchConsole".into(),
            &Value::Bool(patch_console),
        )?;
        let api = engine.call(realm, &installer, &Value::Undefined, &[host])?;
        self.inspect = engine.get(realm, &api, &"inspect".into())?;
        Ok(())
    }

    /// Show a value the way the REPL does.
    pub fn inspect(&mut self, value: &Value) -> String {
        let realm = self.realm();
        let f = self.inspect.clone();
        let engine = self.ctx.engine();
        match engine.call(realm, &f, &Value::Undefined, std::slice::from_ref(value)) {
            Ok(Value::String(s)) => s.to_string_lossy(),
            _ => engine
                .to_string(realm, value)
                .unwrap_or_else(|_| "<value>".into()),
        }
    }

    // ---- running code ------------------------------------------------

    /// Run a classic script file (or `[stdin]`, `[eval]`), then the event
    /// loop. With `show`, the completion value is returned for printing.
    pub fn run_script(&mut self, name: &str, source: &str) -> Result<Shown, Failure> {
        if let Some(map) = host::source_map_for(name, source) {
            let _ = self.ctx.engine().set_source_map(name, Some(&map));
        }
        self.main_source = Some((name.to_string(), source.to_string()));
        let realm = self.realm();
        let r = self.ctx.engine().eval_script(realm, name, source);
        let v = self.check(r)?;
        self.run_event_loop()?;
        Ok(Shown::Value(v))
    }

    /// Load the module `specifier` (a path, or a virtual module the host
    /// serves), then run the event loop and report how its evaluation ended.
    pub fn run_module(&mut self, specifier: &str, source: &str) -> Result<Shown, Failure> {
        self.main_source = Some((specifier.to_string(), source.to_string()));
        let realm = self.realm();
        let r = self.ctx.engine().load_module(realm, specifier);
        let p = self.check(r)?;
        self.await_main(p)
    }

    /// Run a container written by `agnijs compile`, read from `name` (a
    /// path, or `[stdin]`). A module goes through the host's loader as
    /// [`api::ModuleSource::Bytecode`], so its imports load like a source
    /// module's.
    pub fn run_bytecode(&mut self, name: &str, bytes: &[u8]) -> Result<Shown, Failure> {
        if is_module_bytecode(self.ctx.engine(), bytes) {
            if !host::is_path(name) {
                self.host
                    .add_virtual_module(name, api::ModuleSource::Bytecode(bytes.to_vec()));
            }
            let realm = self.realm();
            let r = self.ctx.engine().load_module(realm, name);
            let p = self.check(r)?;
            return self.await_main(p);
        }
        let realm = self.realm();
        let r = self.ctx.engine().eval_bytecode(realm, bytes);
        let v = self.check(r)?;
        self.run_event_loop()?;
        Ok(Shown::Value(v))
    }

    fn await_main(&mut self, p: Value) -> Result<Shown, Failure> {
        self.main_promise = Some(p.clone());
        let looped = self.run_event_loop();
        let realm = self.realm();
        let state = self.ctx.engine().promise_state(realm, &p);
        self.main_promise = None;
        looped?;
        match state {
            Ok(PromiseState::Fulfilled(_)) => Ok(Shown::Nothing),
            Ok(PromiseState::Rejected(reason)) => {
                self.report_uncaught(&reason, false);
                Err(Failure(exit::UNCAUGHT))
            }
            Ok(PromiseState::Pending) => {
                eprintln!("Warning: Detected unsettled top-level await");
                Err(Failure(exit::UNSETTLED))
            }
            Err(e) => Err(self.report(e)),
        }
    }

    /// Pass a value through, or report the failure.
    pub fn check<T>(&mut self, r: api::Result<T>) -> Result<T, Failure> {
        r.map_err(|e| self.report(e))
    }

    // ---- the event loop -------------------------------------------------

    /// Run promise jobs and timers until nothing is left. Unhandled
    /// rejections are reported once the job queue is empty, as Node does
    /// after each macrotask.
    pub fn run_event_loop(&mut self) -> Result<(), Failure> {
        loop {
            self.drain_jobs()?;
            if !self.fire_next_timer(None)? {
                return Ok(());
            }
        }
    }

    /// Run promise jobs until none is pending, then report unhandled
    /// rejections.
    pub fn drain_jobs(&mut self) -> Result<(), Failure> {
        let realm = self.realm();
        loop {
            let r = self.ctx.engine().run_jobs(realm);
            self.check(r)?;
            if !self.ctx.engine().has_pending_jobs() {
                break;
            }
        }
        let mut rejections = self.host.take_rejections();
        if let Some(main) = &self.main_promise {
            let main = main.clone();
            let engine = self.ctx.engine();
            rejections.retain(|(p, _)| !engine.strict_equals(p, &main));
        }
        if let Some((_, reason)) = rejections.into_iter().next() {
            self.report_uncaught(&reason, true);
            return Err(Failure(exit::UNCAUGHT));
        }
        Ok(())
    }

    /// Wait for the earliest timer and run it. Returns false when there is
    /// none, or (with a deadline) when none is due before it.
    pub fn fire_next_timer(&mut self, deadline: Option<Instant>) -> Result<bool, Failure> {
        let (key, id) = loop {
            let mut t = self.timers.borrow_mut();
            let Some((&key, &id)) = t.queue.iter().next() else {
                return Ok(false);
            };
            if !t.live.contains_key(&id) {
                t.queue.remove(&key);
                continue;
            }
            break (key, id);
        };
        let due = key.0;
        if deadline.is_some_and(|d| d < due) {
            // Sleep until the deadline only; the caller polls again.
            self.sleep_until(deadline.unwrap_or(due))?;
            return Ok(false);
        }
        self.sleep_until(due)?;
        let (callback, args) = {
            let mut t = self.timers.borrow_mut();
            t.queue.remove(&key);
            let Some(timer) = t.live.get(&id) else {
                return Ok(true);
            };
            let call = (timer.callback.clone(), timer.args.clone());
            match timer.repeat {
                Some(every) => t.schedule(id, every),
                None => {
                    t.live.remove(&id);
                }
            }
            call
        };
        let realm = self.realm();
        let r = self
            .ctx
            .engine()
            .call(realm, &callback, &Value::Undefined, &args);
        self.check(r)?;
        Ok(true)
    }

    fn sleep_until(&mut self, due: Instant) -> Result<(), Failure> {
        loop {
            if self.host.take_interrupt() {
                return Err(Failure(exit::INTERRUPTED));
            }
            let now = Instant::now();
            if now >= due {
                return Ok(());
            }
            std::thread::sleep((due - now).min(Duration::from_millis(50)));
        }
    }

    // ---- errors -----------------------------------------------------------

    /// Print an engine failure and pick the exit status.
    pub fn report(&mut self, e: api::Error) -> Failure {
        match e {
            api::Error::Exception(x) => {
                self.report_uncaught(&x.value, false);
                Failure(exit::UNCAUGHT)
            }
            api::Error::OutOfFuel => {
                let budget = self.limits.fuel.unwrap_or(0);
                eprintln!("agnijs: fuel exhausted: the program ran more than --fuel {budget} instructions");
                Failure(exit::LIMIT)
            }
            api::Error::OutOfMemory => {
                let limit = self.limits.max_memory.unwrap_or(0);
                eprintln!("agnijs: memory limit exceeded (--max-memory {limit})");
                Failure(exit::LIMIT)
            }
            api::Error::Interrupted => {
                // The flag may still be set if the engine noticed it first.
                self.host.take_interrupt();
                eprintln!("agnijs: interrupted");
                Failure(exit::INTERRUPTED)
            }
            e @ (api::Error::BytecodeMismatch { .. } | api::Error::BytecodeCorrupt(_)) => {
                // Input that cannot be used, like a file that cannot be read.
                eprintln!("agnijs: {e}");
                Failure(exit::USAGE)
            }
            other => {
                eprintln!("agnijs: {other}");
                Failure(exit::UNCAUGHT)
            }
        }
    }

    /// Print an uncaught exception or rejection: where it was thrown (the
    /// source line under a caret) and its stack in the V8 format, mapped
    /// through any source maps.
    pub fn report_uncaught(&mut self, value: &Value, in_promise: bool) {
        let text = self.error_text(value);
        if self.repl_mode {
            let p = if in_promise {
                "Uncaught (in promise) "
            } else {
                "Uncaught "
            };
            eprintln!("{p}{text}");
            return;
        }
        let mut out = String::new();
        if let Some(frame) = self.code_frame(value) {
            out.push_str(&frame);
            out.push('\n');
        }
        if in_promise {
            out.push_str("Uncaught (in promise) ");
        } else if !matches!(value, Value::Object(_)) {
            out.push_str("Uncaught ");
        }
        out.push_str(&text);
        eprintln!("{out}");
    }

    /// The text of a thrown value: an error's `stack` (plus any extra own
    /// properties), otherwise the inspected value.
    pub fn error_text(&mut self, value: &Value) -> String {
        self.inspect(value)
    }

    /// `file:line`, the source line and a caret under the column of the
    /// innermost frame that has a position, from the error's stack text.
    fn code_frame(&mut self, value: &Value) -> Option<String> {
        let Value::Object(_) = value else {
            return None;
        };
        let realm = self.realm();
        let engine = self.ctx.engine();
        let stack = engine.get(realm, value, &"stack".into()).ok()?;
        let Value::String(stack) = stack else {
            return None;
        };
        let stack = stack.to_string_lossy();
        let (script, line, column) = stack.lines().skip(1).find_map(parse_frame)?;
        if script.starts_with("agnijs:") {
            return None;
        }
        let main = self
            .main_source
            .as_ref()
            .filter(|(name, _)| *name == script)
            .map(|(_, text)| text.clone());
        let engine = self.ctx.engine();
        let source = engine
            .script_source(realm, &script)
            .or(main)
            .or_else(|| std::fs::read_to_string(&script).ok())?;
        let text = source.lines().nth(line.checked_sub(1)?)?;
        let col = column.saturating_sub(1);
        let pad: String = text
            .chars()
            .take(col)
            .map(|c| if c == '\t' { '\t' } else { ' ' })
            .collect();
        Some(format!("{script}:{line}\n{text}\n{pad}^\n"))
    }
}

/// `    at f (file:3:7)` or `    at file:3:7` -> (file, 3, 7).
fn parse_frame(line: &str) -> Option<(String, usize, usize)> {
    let rest = line.trim_start().strip_prefix("at ")?;
    let loc = match (rest.rfind('('), rest.ends_with(')')) {
        (Some(i), true) => &rest[i + 1..rest.len() - 1],
        _ => rest,
    };
    let (rest, col) = loc.rsplit_once(':')?;
    let (file, line) = rest.rsplit_once(':')?;
    let line: usize = line.parse().ok()?;
    let col: usize = col.parse().ok()?;
    if file.is_empty() || line == 0 {
        return None;
    }
    Some((file.to_string(), line, col))
}

/// Whether a container holds a module. The engine's payload starts with a
/// kind byte, script or module (ADR 0010, M7 addendum); a container this
/// engine cannot read is left to `eval_bytecode` to refuse.
fn is_module_bytecode(engine: &mut dyn Engine, bytes: &[u8]) -> bool {
    const KIND_MODULE: u8 = 2;
    let format = engine.info().bytecode_format;
    matches!(
        api::bytecode::unwrap(format, bytes),
        Ok(payload) if payload.first() == Some(&KIND_MODULE)
    )
}

#[cfg(test)]
mod tests {
    use super::parse_frame;

    #[test]
    fn frames_parse() {
        assert_eq!(
            parse_frame("    at f (C:/a/b.js:3:7)"),
            Some(("C:/a/b.js".into(), 3, 7))
        );
        assert_eq!(
            parse_frame("    at /x/y.mjs:10:1"),
            Some(("/x/y.mjs".into(), 10, 1))
        );
        assert_eq!(parse_frame("    at native"), None);
    }
}
