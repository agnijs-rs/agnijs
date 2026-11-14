//! The `agnijs` binary, spawned as a user runs it: files, standard input,
//! `eval`, the REPL over a pipe, modules, errors and exit statuses, limits,
//! bytecode, snapshots, source maps and a DevTools client on `--inspect-brk`.

use agnijs_js_cdp::ws;
use serde_json::{json, Value as Json};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_agnijs");

/// A fresh directory for one test.
fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("agnijs-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write(dir: &Path, file: &str, text: &str) -> PathBuf {
    let p = dir.join(file);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&p, text).unwrap();
    p
}

fn agnijs(cwd: &Path, args: &[&str]) -> Output {
    agnijs_stdin(cwd, args, "")
}

fn agnijs_stdin(cwd: &Path, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("AGNIJS_REPL_HISTORY", "")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n")
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).replace("\r\n", "\n")
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

#[test]
fn runs_a_script_file() {
    let d = dir("script");
    write(
        &d,
        "hello.js",
        "console.log('hello', 1 + 2, { a: [1, 'x'] });\n",
    );
    let o = agnijs(&d, &["hello.js"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "hello 3 { a: [ 1, 'x' ] }\n");
    // The `run` subcommand is the same thing.
    let o = agnijs(&d, &["run", "hello.js"]);
    assert_eq!(out(&o), "hello 3 { a: [ 1, 'x' ] }\n");
}

#[test]
fn an_uncaught_error_prints_a_code_frame_and_a_v8_stack() {
    let d = dir("uncaught");
    write(
        &d,
        "e.js",
        "function inner() {\n  throw new TypeError('bad thing');\n}\ninner();\n",
    );
    let o = agnijs(&d, &["e.js"]);
    assert_eq!(code(&o), 1);
    let e = err(&o);
    assert!(
        e.contains("e.js:2\n  throw new TypeError('bad thing');\n"),
        "{e}"
    );
    assert!(e.contains("TypeError: bad thing\n    at inner ("), "{e}");
    assert!(e.contains("e.js:2:"), "{e}");
    assert!(e.contains("e.js:4:1"), "{e}");
}

#[test]
fn a_syntax_error_shows_where_it_is() {
    let d = dir("syntax");
    write(&d, "s.js", "let a = 1;\nlet b = a +* 2;\n");
    let o = agnijs(&d, &["s.js"]);
    assert_eq!(code(&o), 1);
    let e = err(&o);
    assert!(e.contains("s.js:2\nlet b = a +* 2;\n           ^\n"), "{e}");
    assert!(e.contains("SyntaxError:"), "{e}");
}

/// The position comes from the engine's error (TASK-103.24), so it is
/// shown for code that is not a file and for an imported module too.
#[test]
fn a_syntax_error_in_eval_code_or_an_imported_module_shows_where_it_is() {
    let d = dir("syntax-more");
    let o = agnijs(&d, &["eval", "let x = 1;\nx +* 2"]);
    assert_eq!(code(&o), 1);
    let e = err(&o);
    assert!(e.contains("[eval]:2\nx +* 2\n   ^\n"), "{e}");
    write(&d, "main.mjs", "import './dep.mjs';\n");
    write(&d, "dep.mjs", "export const a = 1;\nexport const = 2;\n");
    let o = agnijs(&d, &["main.mjs"]);
    assert_eq!(code(&o), 1);
    let e = err(&o);
    assert!(e.contains("dep.mjs:2\nexport const = 2;\n"), "{e}");
    assert!(e.contains("SyntaxError:"), "{e}");
}

#[test]
fn an_unhandled_rejection_fails_the_run() {
    let d = dir("rejection");
    let o = agnijs(&d, &["eval", "Promise.reject(new RangeError('nope'))"]);
    assert_eq!(code(&o), 1);
    assert!(
        err(&o).contains("Uncaught (in promise) RangeError: nope"),
        "{}",
        err(&o)
    );
    // A rejection handled later in the same turn is fine.
    let o = agnijs(
        &d,
        &[
            "eval",
            "const p = Promise.reject(1); p.catch(() => console.log('caught'))",
        ],
    );
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "caught\n");
}

#[test]
fn modules_import_files_and_json_and_await_timers() {
    let d = dir("modules");
    write(&d, "lib/math.mjs", "export const twice = (x) => x * 2;\n");
    write(&d, "data.json", "{\"n\": 21}");
    write(
        &d,
        "main.mjs",
        "import { twice } from './lib/math.mjs';\n\
         import data from './data.json' with { type: 'json' };\n\
         const v = await new Promise((r) => setTimeout(() => r(twice(data.n)), 10));\n\
         console.log(v, import.meta.url.startsWith('file:///'), scriptArgs.slice(1));\n",
    );
    let o = agnijs(&d, &["main.mjs", "a", "--b"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "42 true [ 'a', '--b' ]\n");
}

#[test]
fn node_modules_packages_resolve() {
    let d = dir("packages");
    write(
        &d,
        "node_modules/pkg/package.json",
        r#"{"name": "pkg", "exports": {".": {"import": "./esm.js"}}}"#,
    );
    write(
        &d,
        "node_modules/pkg/esm.js",
        "export default 'from pkg';\n",
    );
    write(&d, "package.json", r#"{"type": "module"}"#);
    write(&d, "app.js", "import v from 'pkg';\nconsole.log(v);\n");
    let o = agnijs(&d, &["app.js"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "from pkg\n");
}

#[test]
fn script_or_module_is_decided_by_flag_extension_and_syntax() {
    let d = dir("mode");
    write(&d, "lib.mjs", "export const x = 1;\n");
    // No package.json: the syntax decides.
    write(
        &d,
        "detect.js",
        "import { x } from './lib.mjs';\nconsole.log('module', x);\n",
    );
    let o = agnijs(&d, &["detect.js"]);
    assert_eq!(out(&o), "module 1\n", "{}", err(&o));
    // `this` at the top level tells them apart.
    write(&d, "this.js", "console.log(typeof this);\n");
    assert_eq!(out(&agnijs(&d, &["this.js"])), "object\n");
    assert_eq!(out(&agnijs(&d, &["--module", "this.js"])), "undefined\n");
    write(&d, "this.mjs", "console.log(typeof this);\n");
    assert_eq!(out(&agnijs(&d, &["this.mjs"])), "undefined\n");
    assert_eq!(out(&agnijs(&d, &["--script", "this.mjs"])), "object\n");
}

#[test]
fn an_unsettled_top_level_await_exits_13() {
    let d = dir("unsettled");
    write(&d, "hang.mjs", "await new Promise(() => {});\n");
    let o = agnijs(&d, &["hang.mjs"]);
    assert_eq!(code(&o), 13);
    assert!(err(&o).contains("unsettled top-level await"), "{}", err(&o));
}

#[test]
fn a_module_that_throws_fails_with_its_error() {
    let d = dir("module-throw");
    write(
        &d,
        "t.mjs",
        "export {};\nthrow new Error('from a module');\n",
    );
    let o = agnijs(&d, &["t.mjs"]);
    assert_eq!(code(&o), 1);
    let e = err(&o);
    assert!(e.contains("Error: from a module"), "{e}");
    // Reported once, as the module's failure, not also as a rejection.
    assert!(!e.contains("in promise"), "{e}");
}

#[test]
fn standard_input_runs_as_a_script_or_a_module() {
    let d = dir("stdin");
    write(&d, "lib.mjs", "export const x = 'imported';\n");
    let o = agnijs_stdin(&d, &[], "console.log('from stdin', 6 * 7)\n");
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "from stdin 42\n");
    let o = agnijs_stdin(
        &d,
        &["run", "-"],
        "import { x } from './lib.mjs';\nconsole.log(x);\n",
    );
    assert_eq!(out(&o), "imported\n", "{}", err(&o));
}

#[test]
fn eval_runs_code_and_print_shows_the_value() {
    let d = dir("eval");
    let o = agnijs(&d, &["eval", "-p", "({ n: 1n, s: new Set([1]), f() {} })"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "{ n: 1n, s: Set(1) { 1 }, f: [Function: f] }\n");
    let o = agnijs(&d, &["eval", "--module", "console.log(typeof this)"]);
    assert_eq!(out(&o), "undefined\n");
    let o = agnijs(&d, &["eval", "throw 'a string'"]);
    assert_eq!(code(&o), 1);
    assert!(err(&o).contains("Uncaught 'a string'"), "{}", err(&o));
}

#[test]
fn timers_run_in_order_and_can_be_cleared() {
    let d = dir("timers");
    let o = agnijs(
        &d,
        &[
            "eval",
            "setTimeout(() => console.log('b'), 20); setTimeout(() => console.log('a'), 5);\n\
             const t = setTimeout(() => console.log('never'), 1); clearTimeout(t);\n\
             let n = 0; const i = setInterval(() => { console.log('tick', ++n); if (n === 2) clearInterval(i); }, 1);\n\
             queueMicrotask(() => console.log('micro'));",
        ],
    );
    assert_eq!(code(&o), 0, "{}", err(&o));
    let lines: Vec<String> = out(&o).lines().map(String::from).collect();
    assert_eq!(lines[0], "micro");
    assert!(lines.contains(&"tick 2".to_string()));
    assert!(!lines.contains(&"never".to_string()));
    let a = lines.iter().position(|l| l == "a").unwrap();
    let b = lines.iter().position(|l| l == "b").unwrap();
    assert!(a < b, "{lines:?}");
}

#[test]
fn the_fuel_budget_stops_a_runaway_program() {
    let d = dir("fuel");
    let o = agnijs(&d, &["--fuel", "10000", "eval", "while (true) {}"]);
    // Flags before the subcommand are the bare form's; give them after it.
    assert_ne!(code(&o), 0);
    let o = agnijs(&d, &["eval", "--fuel", "10000", "while (true) {}"]);
    assert_eq!(code(&o), 3);
    assert!(err(&o).contains("fuel exhausted"), "{}", err(&o));
    // JavaScript cannot catch it.
    let o = agnijs(
        &d,
        &[
            "eval",
            "--fuel",
            "10000",
            "try { for (;;) {} } catch (e) { console.log('caught') }",
        ],
    );
    assert_eq!(code(&o), 3);
    assert_eq!(out(&o), "");
    // A program within budget runs normally.
    let o = agnijs(&d, &["eval", "--fuel", "100000", "-p", "1 + 1"]);
    assert_eq!(out(&o), "2\n");
}

#[test]
fn the_memory_limit_stops_a_growing_heap() {
    let d = dir("memory");
    let o = agnijs(
        &d,
        &[
            "eval",
            "--max-memory",
            "8M",
            "const a = []; for (;;) a.push(new Array(1000).fill(0));",
        ],
    );
    assert_eq!(code(&o), 3, "{}", err(&o));
    assert!(err(&o).contains("memory limit"), "{}", err(&o));
}

/// Deeply nested source and JSON end in an uncaught error, not a stack
/// overflow that kills the process (TASK-103.27). The CLI runs on the main
/// thread, 1 MiB on Windows.
#[test]
fn deeply_nested_input_is_an_error_not_a_crash() {
    let d = dir("deep");
    let deep = 300_000;
    let src = format!("{}1{}", "[".repeat(deep), "]".repeat(deep));
    let file = write(&d, "deep.js", &src);
    let o = agnijs(&d, &[file.to_str().unwrap()]);
    let e = err(&o);
    assert_eq!(code(&o), 1, "{e}");
    assert!(
        e.contains("SyntaxError: Maximum call stack size exceeded"),
        "{e}"
    );
    let json = format!("JSON.parse('['.repeat({deep}) + ']'.repeat({deep}))");
    let o = agnijs(&d, &["eval", &json]);
    let e = err(&o);
    assert_eq!(code(&o), 1, "{e}");
    assert!(
        e.contains("RangeError: Maximum call stack size exceeded"),
        "{e}"
    );
}

#[test]
fn the_seed_makes_math_random_reproducible() {
    let d = dir("seed");
    let run = || {
        out(&agnijs(
            &d,
            &[
                "eval",
                "--seed",
                "7",
                "-p",
                "[Math.random(), Math.random()]",
            ],
        ))
    };
    assert_eq!(run(), run());
    let o = agnijs(
        &d,
        &[
            "eval",
            "--regex-matcher",
            "linear-when-eligible",
            "-p",
            "/(a+)+b/.test('aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaac')",
        ],
    );
    assert_eq!(out(&o), "false\n", "{}", err(&o));
}

#[test]
fn a_source_map_rewrites_the_stack() {
    let d = dir("sourcemap");
    // gen.js 1:18 -> src/orig.ts 10:3 and 2:0 -> 20:0 (0-based).
    write(
        &d,
        "gen.js",
        "function boom() { throw new Error('m') }\nboom();\n//# sourceMappingURL=gen.js.map\n",
    );
    write(
        &d,
        "gen.js.map",
        r#"{"version":3,"sources":["orig.ts"],"sourceRoot":"src","names":[],"mappings":"AAAA,kBASG;AAUH"}"#,
    );
    let o = agnijs(&d, &["gen.js"]);
    assert_eq!(code(&o), 1);
    let e = err(&o);
    assert!(
        e.contains("Error: m\n    at boom (src/orig.ts:10:4)\n    at src/orig.ts:20:1"),
        "{e}"
    );
}

/// A `data:` source map URL with a multi-byte character after `%` is read
/// (and ignored, as it is no source map) instead of panicking, in a script
/// and in a module loaded by `import()` (TASK-103.37).
#[test]
fn a_source_map_url_with_a_bad_escape_does_not_crash() {
    let d = dir("sourcemap-escape");
    let comment = "//# sourceMappingURL=data:,%a\u{e9}1";
    write(&d, "pct.js", &format!("console.log('ran');\n{comment}\n"));
    let o = agnijs(&d, &["pct.js"]);
    assert_eq!((code(&o), out(&o)), (0, "ran\n".to_string()), "{}", err(&o));
    write(&d, "dep.mjs", &format!("export const x = 1;\n{comment}\n"));
    write(
        &d,
        "main.mjs",
        "await import('./dep.mjs').then((m) => console.log('loaded', m.x), (e) => console.log('caught', e));\n",
    );
    let o = agnijs(&d, &["main.mjs"]);
    assert_eq!(
        (code(&o), out(&o)),
        (0, "loaded 1\n".to_string()),
        "{}",
        err(&o)
    );
}

#[test]
fn compiled_bytecode_runs_scripts_and_modules() {
    let d = dir("bytecode");
    write(
        &d,
        "s.js",
        "console.log('script', [1, 2].map((x) => x * 3));\n",
    );
    let o = agnijs(&d, &["compile", "s.js"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let bytes = std::fs::read(d.join("s.jsc")).unwrap();
    assert!(bytes.starts_with(b"AGJS"));
    assert_eq!(out(&agnijs(&d, &["s.jsc"])), "script [ 3, 6 ]\n");
    write(&d, "lib.mjs", "export const v = 'lib';\n");
    write(
        &d,
        "m.mjs",
        "import { v } from './lib.mjs';\nconsole.log('module', v, await 1);\n",
    );
    let o = agnijs(&d, &["compile", "m.mjs", "-o", "m.bin"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let o = agnijs(&d, &["m.bin"]);
    assert_eq!(out(&o), "module lib 1\n", "{}", err(&o));
    // Damaged bytecode is refused, not run.
    let mut bad = std::fs::read(d.join("s.jsc")).unwrap();
    let last = bad.len() - 1;
    bad[last] ^= 0xff;
    std::fs::write(d.join("bad.jsc"), bad).unwrap();
    let o = agnijs(&d, &["bad.jsc"]);
    assert_eq!(code(&o), 2);
    assert!(err(&o).contains("bytecode"), "{}", err(&o));
}

#[test]
fn a_snapshot_restores_a_bootstrapped_realm() {
    let d = dir("snapshot");
    write(&d, "boot.js", "globalThis.greet = (n) => 'hello ' + n;\n");
    let o = agnijs(&d, &["snapshot", "boot.js", "-o", "realm.snap"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let o = agnijs(
        &d,
        &[
            "eval",
            "--snapshot",
            "realm.snap",
            "-p",
            "setTimeout(() => {}, 1), greet('snapshot')",
        ],
    );
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "'hello snapshot'\n");
    let o = agnijs(&d, &["eval", "--snapshot", "boot.js", "1"]);
    assert_eq!(code(&o), 2);
    assert!(err(&o).contains("not a usable snapshot"), "{}", err(&o));
}

#[test]
fn usage_and_file_errors_exit_2() {
    let d = dir("usage");
    assert_eq!(code(&agnijs(&d, &["missing.js"])), 2);
    assert_eq!(code(&agnijs(&d, &["eval"])), 2);
    assert_eq!(code(&agnijs(&d, &["eval", "--fuel", "lots", "1"])), 2);
    let o = agnijs(&d, &["--help"]);
    assert_eq!(code(&o), 0);
    assert!(out(&o).contains("Exit status"));
}

#[test]
fn the_repl_evaluates_multi_line_input_and_awaits() {
    let d = dir("repl");
    let history = d.join("history.txt");
    let input = "1 + 1\n\
                 let o = {\n  a: [1,\n  2]\n}\n\
                 o.a.length\n\
                 function f(x) {\n  return x * 10;\n}\n\
                 f(4)\n\
                 `two\nlines`\n\
                 await new Promise((r) => setTimeout(() => r('awaited'), 10))\n\
                 const z = await Promise.resolve(5)\n\
                 z + 1\n\
                 Promise.resolve(3)\n\
                 nope()\n\
                 'still running'\n\
                 .exit\n\
                 'not reached'\n";
    let mut child = Command::new(BIN)
        .arg("repl")
        .current_dir(&d)
        .env("AGNIJS_REPL_HISTORY", &history)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let o = child.wait_with_output().unwrap();
    assert_eq!(code(&o), 0, "{}", err(&o));
    let stdout = out(&o);
    for expected in [
        "\n2\n",
        "\n2\n",
        "\n40\n",
        "'two\\nlines'",
        "'awaited'",
        "\n6\n",
        "Promise { 3 }",
        "'still running'",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in:\n{stdout}"
        );
    }
    assert!(!stdout.contains("not reached"), "{stdout}");
    assert!(
        err(&o).contains("Uncaught ReferenceError: nope is not defined"),
        "{}",
        err(&o)
    );
    // Every line typed went to the history file.
    let h = std::fs::read_to_string(&history).unwrap();
    assert!(h.contains("const z = await Promise.resolve(5)"), "{h}");
    assert!(h.contains("f(4)"), "{h}");
}

#[test]
fn the_repl_carries_on_after_running_out_of_fuel() {
    let d = dir("repl-limits");
    let o = agnijs_stdin(&d, &["repl", "--fuel", "100000"], "for (;;) {}\n");
    assert_eq!(code(&o), 0);
    assert!(err(&o).contains("fuel exhausted"), "{}", err(&o));
}

// ---- --inspect-brk with a DevTools-protocol client ----------------------

struct Client {
    stream: TcpStream,
    next_id: u64,
    events: VecDeque<Json>,
}

fn http_get(addr: SocketAddr, path: &str) -> String {
    let mut s = TcpStream::connect(addr).unwrap();
    write!(s, "GET {path} HTTP/1.1\r\nHost: {addr}\r\n\r\n").unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out.split_once("\r\n\r\n").unwrap().1.to_string()
}

impl Client {
    fn connect(addr: SocketAddr) -> Client {
        let list: Json = serde_json::from_str(&http_get(addr, "/json/list")).unwrap();
        let url = list[0]["webSocketDebuggerUrl"]
            .as_str()
            .unwrap()
            .to_string();
        let path = url.split_once(&addr.to_string()).unwrap().1.to_string();
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )
        .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut status = String::new();
        reader.read_line(&mut status).unwrap();
        assert!(status.starts_with("HTTP/1.1 101"), "{status}");
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line.trim().is_empty() {
                break;
            }
        }
        Client {
            stream,
            next_id: 0,
            events: VecDeque::new(),
        }
    }

    fn read(&mut self) -> Json {
        let text = ws::read_message(&mut self.stream)
            .expect("read")
            .expect("server closed");
        serde_json::from_str(&text).unwrap()
    }

    fn call(&mut self, method: &str, params: Json) -> Json {
        self.next_id += 1;
        let id = self.next_id;
        let msg = json!({"id": id, "method": method, "params": params}).to_string();
        ws::write_frame(
            &mut self.stream,
            ws::OP_TEXT,
            msg.as_bytes(),
            Some([7, 1, 9, 3]),
        )
        .unwrap();
        loop {
            let m = self.read();
            if m["id"] == json!(id) {
                assert!(m.get("error").is_none(), "{method}: {m}");
                return m["result"].clone();
            }
            self.events.push_back(m);
        }
    }

    fn event(&mut self, method: &str) -> Json {
        if let Some(i) = self.events.iter().position(|e| e["method"] == method) {
            return self.events.remove(i).unwrap()["params"].clone();
        }
        loop {
            let m = self.read();
            if m["method"] == method {
                return m["params"].clone();
            }
            self.events.push_back(m);
        }
    }

    fn close(mut self) {
        let _ = ws::write_frame(
            &mut self.stream,
            ws::OP_CLOSE,
            &1000u16.to_be_bytes(),
            Some([1, 2, 3, 4]),
        );
    }
}

#[test]
fn inspect_brk_pauses_for_a_devtools_client() {
    let d = dir("inspect");
    write(
        &d,
        "app.js",
        "let total = 0;\nfor (let i = 1; i <= 3; i++) total += i;\nconsole.log('total', total);\n",
    );
    let mut child = Command::new(BIN)
        .args(["--inspect-brk=0", "app.js"])
        .current_dir(&d)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The first line names the endpoint: "Debugger listening on ws://ADDR/ID".
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    stderr.read_line(&mut line).unwrap();
    let url = line
        .trim()
        .strip_prefix("Debugger listening on ws://")
        .unwrap_or_else(|| panic!("unexpected: {line}"));
    let addr: SocketAddr = url.split('/').next().unwrap().parse().unwrap();
    let rest = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });

    let mut c = Client::connect(addr);
    c.call("Runtime.enable", json!({}));
    c.call("Debugger.enable", json!({}));
    c.call("Runtime.runIfWaitingForDebugger", json!({}));
    let paused = c.event("Debugger.paused");
    let frame = &paused["callFrames"][0];
    assert!(
        frame["url"].as_str().unwrap().ends_with("app.js"),
        "{paused}"
    );
    assert_eq!(frame["location"]["lineNumber"], json!(0), "{paused}");
    // Step to the console.log line and look at a local.
    let parsed_id = frame["location"]["scriptId"].clone();
    c.call(
        "Debugger.setBreakpoint",
        json!({"location": {"scriptId": parsed_id, "lineNumber": 2}}),
    );
    c.call("Debugger.resume", json!({}));
    let paused = c.event("Debugger.paused");
    assert_eq!(
        paused["callFrames"][0]["location"]["lineNumber"],
        json!(2),
        "{paused}"
    );
    let v = c.call(
        "Debugger.evaluateOnCallFrame",
        json!({"callFrameId": paused["callFrames"][0]["callFrameId"], "expression": "total * 10"}),
    );
    assert_eq!(v["result"]["value"], json!(60.0), "{v}");
    c.call("Debugger.resume", json!({}));
    let console = c.event("Runtime.consoleAPICalled");
    assert_eq!(console["args"][1]["value"], json!(6.0), "{console}");
    c.close();

    let status = child.wait().unwrap();
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    let stderr = rest.join().unwrap();
    assert!(status.success(), "{stderr}");
    assert_eq!(stdout.replace("\r\n", "\n"), "total 6\n");
}
