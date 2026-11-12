# agnijs-cli

The `agnijs` command: runs JavaScript on the AgniJS engine from the command line. It is a host built only on the public embedding layer (the `agnijs` crate and the `Engine`/`Host` interface), with `agnijs-js-resolve` for Node-style module resolution, so it uses the API an embedder would and touches no engine internals. It is a binary for desktops and servers and is not part of the size-trimmed or no-Intl engine builds.

```
cargo install --path crates/agnijs-cli     # or: cargo run -p agnijs-cli -- ARGS
```

## Usage

```
agnijs app.js a b             # run a file; scriptArgs is ['…/app.js', 'a', 'b']
agnijs run app.mjs            # the same, spelled out
cat app.js | agnijs           # standard input, when it is not a terminal (or `agnijs run -`)
agnijs eval "1 + 1"           # evaluate code; -p prints the completion value
agnijs eval -p "[1, 2].map((x) => x * 2)"
agnijs                        # the REPL, on a terminal (or `agnijs repl`)
agnijs compile app.mjs        # write app.jsc (bytecode); `agnijs app.jsc` runs it
agnijs snapshot boot.js -o realm.snap   # run boot scripts, save the realm
agnijs --snapshot realm.snap app.js     # start from it
agnijs --inspect-brk app.js   # wait for DevTools, pause at the first statement
```

**Script or module.** `--module` (`-m`) or `--script` decides. Otherwise `.mjs` is a module and `.cjs` a script, then a `.js` file follows the `type` of the nearest `package.json`, and a file with neither is a module when it only parses as one (it uses `import` or `export`). `eval` and standard input are scripts unless they only parse as modules.

**Modules** resolve the way Node does: relative and absolute paths, `node_modules` packages with `exports`, `imports` and conditions (`import`, `default`), and `.json` files imported `with { type: 'json' }`. `import.meta` has `url`, `filename` and `dirname`. There are no `node:` built-in modules and no CommonJS `require`.

**Globals** beyond the language: `console` (formatted the way Node's is, with `%s %d %i %f %j %o %O %c`), `setTimeout`, `setInterval`, `clearTimeout`, `clearInterval`, `queueMicrotask` and `scriptArgs`. The process stays alive while timers are pending, as Node's does.

**Errors.** An uncaught exception prints a code frame and the V8-format stack trace, and exits 1:

```
/work/app.js:2
  throw new TypeError('bad thing');
  ^

TypeError: bad thing
    at inner (/work/app.js:2:9)
    at /work/app.js:4:1
```

A syntax error points at its position. A promise rejected with no handler by the end of its turn is reported as `Uncaught (in promise) …` and also exits 1. When a file ends with `//# sourceMappingURL=` (an inline `data:` URL or a file beside it), stack traces show the original positions.

### Options

| Option | Meaning |
| --- | --- |
| `--fuel N` | Stop after N bytecode instructions: deterministic, and not catchable by JavaScript (exit 3) |
| `--max-memory BYTES` | Cap the heap (`K`, `M`, `G` suffixes; exit 3 when reached) |
| `--seed N` | Seed `Math.random`, for reproducible runs |
| `--regex-matcher auto\|backtracking-only\|linear-when-eligible` | The regular-expression matcher policy |
| `--snapshot FILE` | Start from a realm snapshot written by `agnijs snapshot` |
| `--inspect[=[HOST:]PORT]` | Serve the Chrome DevTools Protocol, default `127.0.0.1:9229` (port `0` picks a free one) |
| `--inspect-brk[=[HOST:]PORT]` | The same, and wait for a client, then pause at the first statement of the program |
| `-m`, `--module` / `--script` | Treat the code as a module or a classic script |

The options go after the subcommand (`agnijs eval --fuel 1000 …`), or before the file in the bare form (`agnijs --fuel 1000 app.js`). `agnijs compile` takes `--module`/`--script` and `-o FILE`. `agnijs snapshot` takes one or more scripts, `-o FILE` and `--regex-matcher`.

### Exit status

| Status | Meaning |
| --- | --- |
| 0 | Success |
| 1 | Uncaught exception or unhandled rejection |
| 2 | Usage error, unreadable file, or bytecode or snapshot that cannot be used |
| 3 | Fuel or memory limit reached |
| 13 | The main module's top-level `await` never settled |
| 130 | Interrupted (Ctrl-C) |

## The REPL

Input that is not finished yet (an open bracket, an unterminated template or comment) continues on a `...` line. Values print the way `console.log` shows them, with `Promise { … }` for promises. `await` works at the top level: an awaited expression prints its result, and `const x = await …` defines `x` for later lines. Pending timers and jobs run between lines. Errors print as `Uncaught …` and the REPL carries on.

Commands: `.help`, `.clear` (drop the current multi-line input), `.load FILE`, `.exit`. Ctrl-C clears the line, and a second Ctrl-C on an empty line exits. So does Ctrl-D.

History is saved to `~/.agnijs_repl_history`. The `AGNIJS_REPL_HISTORY` environment variable names another file, and an empty value turns history off.

## Debugging

`--inspect` and `--inspect-brk` serve the Chrome DevTools Protocol over the engine's debugger interface (ADR 0017). They print the WebSocket URL and connect from `chrome://inspect` or a `devtools://` URL. Breakpoints, stepping, scopes, evaluation on a paused frame and console messages work. Once the program is done, a connected client keeps the process alive until it disconnects, as Node does.

## Limitations

- Time zone: `Date` uses UTC, as the engine does everywhere.
- Under `--inspect`, `console` passes its arguments to DevTools as values, so the terminal shows the engine's plain string form, not the formatted one.
- REPL top-level `await` with declarations handles simple `let`/`const`/`var x = …` statements (they become globals). Destructuring declarations together with `await` are not hoisted.
- Output from timers that fire while the REPL waits for input may appear beside the prompt.
- No Node built-in modules, `require`, `process` or file-system APIs: this is an engine shell, not a Node replacement.
