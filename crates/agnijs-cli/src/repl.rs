//! The REPL: a line editor with history on its own thread, so timers,
//! promise jobs and the debugger keep being served while it waits for
//! input; multi-line input; top-level `await`.

use crate::runtime::{Failure, Runtime};
use agnijs::api::{self, PromiseState, Value};
use rustyline::error::ReadlineError;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

const HELP: &str = "\
.help     Show this help
.clear    Discard the input typed so far
.load F   Evaluate the file F in this session
.exit     Leave the REPL (also Ctrl+D, or Ctrl+C twice)

Input that is not complete yet (an open bracket, string or template)
continues on the next line. `await` works at the top level.";

enum Input {
    Line(String),
    Interrupted,
    Eof,
}

/// Where history is kept: `AGNIJS_REPL_HISTORY` (empty for none), else
/// `.agnijs_repl_history` in the home directory.
fn history_path() -> Option<PathBuf> {
    match std::env::var_os("AGNIJS_REPL_HISTORY") {
        Some(p) if p.is_empty() => None,
        Some(p) => Some(PathBuf::from(p)),
        None => std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(|h| PathBuf::from(h).join(".agnijs_repl_history")),
    }
}

/// The line editor thread: it reads one line per prompt it is sent.
fn spawn_reader() -> (mpsc::Sender<String>, mpsc::Receiver<Input>) {
    let (prompt_tx, prompt_rx) = mpsc::channel::<String>();
    let (line_tx, line_rx) = mpsc::channel::<Input>();
    std::thread::spawn(move || {
        let history = history_path();
        let mut editor = match rustyline::DefaultEditor::new() {
            Ok(e) => e,
            Err(e) => {
                eprintln!("agnijs: cannot start the line editor: {e}");
                let _ = line_tx.send(Input::Eof);
                return;
            }
        };
        if let Some(h) = &history {
            let _ = editor.load_history(h);
        }
        while let Ok(prompt) = prompt_rx.recv() {
            let input = match editor.readline(&prompt) {
                Ok(line) => {
                    if !line.trim().is_empty() {
                        let _ = editor.add_history_entry(line.as_str());
                        if let Some(h) = &history {
                            let _ = editor.append_history(h);
                        }
                    }
                    Input::Line(line)
                }
                Err(ReadlineError::Interrupted) => Input::Interrupted,
                Err(_) => Input::Eof,
            };
            let eof = matches!(input, Input::Eof);
            if line_tx.send(input).is_err() || eof {
                break;
            }
        }
    });
    (prompt_tx, line_rx)
}

pub fn run(rt: &mut Runtime) -> i32 {
    rt.repl_mode = true;
    println!(
        "Welcome to AgniJS {}. Type \".help\" for more information.",
        env!("CARGO_PKG_VERSION")
    );
    let (prompt, lines) = spawn_reader();
    let mut buffer = String::new();
    let mut count = 0u32;
    let mut interrupted_once = false;
    let _ = prompt.send("> ".into());
    loop {
        let input = match lines.recv_timeout(Duration::from_millis(20)) {
            Ok(i) => i,
            Err(RecvTimeoutError::Timeout) => {
                idle(rt);
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let line = match input {
            Input::Eof => break,
            Input::Interrupted => {
                if !buffer.is_empty() {
                    buffer.clear();
                } else if interrupted_once {
                    break;
                } else {
                    println!("(To exit, press Ctrl+C again or Ctrl+D or type .exit)");
                    interrupted_once = true;
                }
                let _ = prompt.send("> ".into());
                continue;
            }
            Input::Line(l) => l,
        };
        interrupted_once = false;
        if buffer.is_empty() {
            let cmd = line.trim();
            if let Some(rest) = cmd.strip_prefix('.') {
                let (name, arg) = rest.split_once(' ').unwrap_or((rest, ""));
                let handled = match name {
                    "exit" => break,
                    "help" => {
                        println!("{HELP}");
                        true
                    }
                    "clear" => true,
                    "load" => {
                        match std::fs::read_to_string(arg.trim()) {
                            Ok(src) => {
                                count += 1;
                                evaluate(rt, &src, count);
                            }
                            Err(e) => eprintln!("agnijs: cannot read {}: {e}", arg.trim()),
                        }
                        true
                    }
                    _ => false,
                };
                if handled {
                    let _ = prompt.send("> ".into());
                    continue;
                }
            }
        } else if line.trim() == ".clear" {
            buffer.clear();
            let _ = prompt.send("> ".into());
            continue;
        }
        buffer.push_str(&line);
        buffer.push('\n');
        if buffer.trim().is_empty() {
            buffer.clear();
            let _ = prompt.send("> ".into());
            continue;
        }
        if is_incomplete(rt, &buffer) {
            let _ = prompt.send("... ".into());
            continue;
        }
        count += 1;
        let src = std::mem::take(&mut buffer);
        evaluate(rt, &src, count);
        let _ = prompt.send("> ".into());
    }
    drop(prompt);
    0
}

/// Between inputs: run due timers and pending jobs (and, under
/// `--inspect`, the protocol messages: every engine entry serves them).
fn idle(rt: &mut Runtime) {
    let now = Instant::now();
    loop {
        match rt.fire_next_timer(Some(now)) {
            Ok(true) => {}
            Ok(false) => break,
            Err(_) => break,
        }
        if rt.drain_jobs().is_err() {
            break;
        }
    }
    let _ = rt.drain_jobs();
}

/// Input that fails to parse only because it ended too soon.
fn is_incomplete(rt: &mut Runtime, src: &str) -> bool {
    let realm = rt.realm();
    match rt.ctx.engine().compile_script(realm, "REPL", src) {
        Ok(_) => false,
        Err(api::Error::Exception(e)) if e.name == "SyntaxError" => {
            let m = e.message.to_ascii_lowercase();
            if m.contains("unexpected end of input")
                || m.contains("unterminated template")
                || m.contains("unterminated comment")
            {
                return true;
            }
            // `await` at the top level parses inside an async function.
            src.contains("await")
                && wrap_async(src).is_some_and(|w| {
                    matches!(
                        rt.ctx.engine().compile_script(realm, "REPL", &w),
                        Err(api::Error::Exception(e))
                            if e.message.to_ascii_lowercase().contains("unexpected end of input")
                    )
                })
        }
        Err(_) => false,
    }
}

/// Input with a top-level `await`, as an async function call. A single
/// expression keeps its value; statements become the function body, with
/// simple top-level `let`/`const`/`var x =` declarations made global so
/// they outlive the call.
fn wrap_async(src: &str) -> Option<String> {
    let trimmed = src.trim().trim_end_matches(';');
    if trimmed.is_empty() {
        return None;
    }
    Some(format!("(async () => {{ return (\n{trimmed}\n) }})()"))
}

fn wrap_async_statements(src: &str) -> String {
    let mut body = String::new();
    for line in src.lines() {
        let indent = line.len() - line.trim_start().len();
        let rest = &line[indent..];
        let hoisted = ["let ", "const ", "var "].iter().find_map(|kw| {
            let after = rest.strip_prefix(kw)?;
            let name_len = after
                .char_indices()
                .find(|(_, c)| !(c.is_alphanumeric() || *c == '_' || *c == '$'))
                .map(|(i, _)| i)
                .unwrap_or(after.len());
            let (name, tail) = after.split_at(name_len);
            (!name.is_empty() && tail.trim_start().starts_with('=') && indent == 0)
                .then(|| format!("globalThis.{name}{tail}"))
        });
        body.push_str(hoisted.as_deref().unwrap_or(line));
        body.push('\n');
    }
    format!("(async () => {{\n{body}}})()")
}

fn evaluate(rt: &mut Runtime, src: &str, n: u32) {
    rt.host.take_interrupt();
    let name = format!("REPL{n}");
    let realm = rt.realm();
    let compiles = rt.ctx.engine().compile_script(realm, &name, src).is_ok();
    let (code, awaited) = if !compiles && src.contains("await") {
        let expr =
            wrap_async(src).filter(|w| rt.ctx.engine().compile_script(realm, &name, w).is_ok());
        match expr {
            Some(w) => (w, true),
            None => (wrap_async_statements(src), true),
        }
    } else {
        (src.to_string(), false)
    };
    let r = rt.ctx.engine().eval_script(realm, &name, &code);
    let Ok(value) = rt.check(r) else {
        return;
    };
    let value = if awaited {
        match settle(rt, &value) {
            Some(v) => v,
            None => return,
        }
    } else {
        if rt.drain_jobs().is_err() {
            return;
        }
        value
    };
    let text = rt.inspect(&value);
    println!("{text}");
}

/// Run jobs and timers until `promise` settles; its value, or `None` after
/// reporting its rejection (or an interrupt).
fn settle(rt: &mut Runtime, promise: &Value) -> Option<Value> {
    rt.main_promise = Some(promise.clone());
    let r = settle_inner(rt, promise);
    rt.main_promise = None;
    r
}

fn settle_inner(rt: &mut Runtime, promise: &Value) -> Option<Value> {
    let realm = rt.realm();
    loop {
        if rt.drain_jobs().is_err() {
            return None;
        }
        match rt.ctx.engine().promise_state(realm, promise) {
            Ok(PromiseState::Fulfilled(v)) => return Some(v),
            Ok(PromiseState::Rejected(r)) => {
                rt.report_uncaught(&r, false);
                return None;
            }
            Ok(PromiseState::Pending) => {}
            Err(e) => {
                rt.report(e);
                return None;
            }
        }
        match rt.fire_next_timer(None) {
            Ok(true) => {}
            Ok(false) => {
                println!("Promise {{ <pending> }}");
                return None;
            }
            Err(Failure(_)) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statements_hoist_simple_declarations() {
        let w = wrap_async_statements("const x = await f();\nif (x) {\n  let y = 1;\n}\n");
        assert!(w.contains("globalThis.x = await f();"));
        assert!(w.contains("  let y = 1;"));
    }
}
