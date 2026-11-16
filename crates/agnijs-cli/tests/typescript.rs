//! TypeScript files on the command, built with the `typescript` feature
//! (ADR 0029); without it a `.ts` file is JavaScript, as before.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_agnijs");

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("agnijs-cli-ts-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write(dir: &Path, file: &str, text: &str) {
    let p = dir.join(file);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn agnijs(cwd: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("AGNIJS_REPL_HISTORY", "")
        .output()
        .unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n")
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).replace("\r\n", "\n")
}

#[cfg(not(feature = "typescript"))]
#[test]
fn without_the_feature_a_ts_file_is_javascript() {
    let d = dir("off");
    write(&d, "plain.ts", "console.log(1 + 1);\n");
    let o = agnijs(&d, &["plain.ts"]);
    assert_eq!(out(&o), "2\n", "{}", err(&o));
    write(&d, "typed.ts", "let x: number = 1;\n");
    let o = agnijs(&d, &["typed.ts"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("SyntaxError:"), "{}", err(&o));
}

#[cfg(feature = "typescript")]
mod with_the_feature {
    use super::*;

    #[test]
    fn modules_scripts_and_jsx_run() {
        let d = dir("run");
        write(
            &d,
            "main.ts",
            "import { area, type Shape } from './shapes.ts';\n\
             import { twice } from './lib/util';\n\
             enum Unit { Cm = 'cm' }\n\
             const s: Shape = { w: 2, h: 3 };\n\
             console.log(twice(area(s)), Unit.Cm, import.meta.url.endsWith('/main.ts'));\n",
        );
        write(
            &d,
            "shapes.ts",
            "export interface Shape { w: number; h: number }\n\
             export function area({ w, h }: Shape): number { return w * h; }\n",
        );
        // Found without its extension, as tsc's bundler resolution does.
        write(
            &d,
            "lib/util.ts",
            "export const twice = (n: number): number => n * 2;\n",
        );
        let o = agnijs(&d, &["main.ts"]);
        assert_eq!(out(&o), "12 cm true\n", "{}", err(&o));

        write(
            &d,
            "m.mts",
            "const x: number = await Promise.resolve(5);\nconsole.log(x);\n",
        );
        let o = agnijs(&d, &["m.mts"]);
        assert_eq!(out(&o), "5\n", "{}", err(&o));

        // A .cts file is a script: `this` at top level is the global object.
        write(
            &d,
            "s.cts",
            "var g: boolean = this === globalThis;\nconsole.log(g);\n",
        );
        let o = agnijs(&d, &["s.cts"]);
        assert_eq!(out(&o), "true\n", "{}", err(&o));

        write(
            &d,
            "view.tsx",
            "const React = { createElement: (t: string, p: any, ...c: string[]) => `<${t}>${c.join('')}</${t}>` };\n\
             const who: string = 'ts';\n\
             console.log(<b>{who}</b>);\n",
        );
        let o = agnijs(&d, &["view.tsx"]);
        assert_eq!(out(&o), "<b>ts</b>\n", "{}", err(&o));
    }

    #[test]
    fn an_uncaught_error_shows_typescript_lines() {
        let d = dir("uncaught");
        write(
            &d,
            "e.ts",
            "type Id = number;\n\
             enum Kind { A, B }\n\
             function check(id: Id, kind: Kind = Kind.A): void {\n\
             \x20 if (id < 0) throw new RangeError(`bad id ${id}`);\n\
             }\n\
             check(-1);\n",
        );
        let o = agnijs(&d, &["e.ts"]);
        assert_eq!(o.status.code(), Some(1));
        let e = err(&o);
        // The code frame quotes the TypeScript, and the stack points into it.
        assert!(
            e.contains("e.ts:4\n  if (id < 0) throw new RangeError(`bad id ${id}`);\n"),
            "{e}"
        );
        assert!(e.contains("RangeError: bad id -1\n    at check ("), "{e}");
        assert!(e.contains("e.ts:4:"), "{e}");
        assert!(e.contains("e.ts:6:1"), "{e}");
    }

    #[test]
    fn diagnostics_show_where_they_are() {
        let d = dir("diagnostics");
        write(&d, "bad.ts", "let a: number = 1;\nlet b: = a;\n");
        let o = agnijs(&d, &["bad.ts"]);
        assert_eq!(o.status.code(), Some(1));
        let e = err(&o);
        assert!(e.contains("bad.ts:2\nlet b: = a;\n       ^\n"), "{e}");
        assert!(e.contains("bad.ts:2:8: "), "{e}");

        // A construct the crate refuses, with its help, in an imported module.
        write(&d, "main.ts", "import './dep.ts';\n");
        write(&d, "dep.ts", "export {};\nclass A { accessor x = 1; }\n");
        let o = agnijs(&d, &["main.ts"]);
        assert_eq!(o.status.code(), Some(1));
        let e = err(&o);
        assert!(
            e.contains("dep.ts:2:11: `accessor` fields are not supported"),
            "{e}"
        );
        assert!(e.contains("help: use a getter and a setter"), "{e}");
    }

    #[test]
    fn compile_refuses_typescript() {
        let d = dir("compile");
        write(&d, "a.ts", "let x: number = 1;\n");
        let o = agnijs(&d, &["compile", "a.ts"]);
        assert_eq!(o.status.code(), Some(2));
        assert!(err(&o).contains("compile takes JavaScript"), "{}", err(&o));
    }
}
