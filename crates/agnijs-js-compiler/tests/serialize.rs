//! Serialised bytecode: a compiled code tree survives encode and decode
//! unchanged, and bad input is refused with an error instead of a panic.

use agnijs_js_compiler::serialize::{decode, encode, DecodeError};
use agnijs_js_compiler::{
    compile_module, compile_script, BinOp, CodeBlock, DebugInfo, DebugSection, Instr,
};
use std::rc::Rc;

/// The compiler's own output, printed in full, is the reference: a decoded
/// tree must print identically (instructions, constants, scopes, handlers,
/// declarations, module records, offsets, nested functions).
fn assert_round_trips(code: &CodeBlock) {
    let bytes = encode(code);
    let back = decode(&bytes).unwrap_or_else(|e| panic!("decode failed: {e}"));
    assert_eq!(format!("{code:#?}"), format!("{back:#?}"));
}

const SCRIPT: &str = r#"
var g = 1; let l = 2; const c = 3;
function plain(a, b = 2, ...rest) { return a + b + rest.length + arguments.length; }
function* gen() { const x = yield 1; try { yield x; } finally { g++; } }
async function af() { await null; for await (const v of []) {} return 1; }
async function* ag() { yield* gen(); }
class Base { #p = 1; static s = 2; get p() { return this.#p; } static { g++; } }
class Derived extends Base { constructor() { super(); this.q = super.p; } m() { return 1; } }
const arrow = (x) => x * 2n ** 70n;
const tag = (s, ...v) => s.raw.join('|');
tag`a${1}b\u{41}${2}`;
const re = /a(?<n>b)+/giu;
label: for (const k in { a: 1 }) { for (const v of [1, 2]) { if (v) continue label; } }
try { throw new Error('x'); } catch ({ message }) { g = message; }
with ({}) { g; }
switch (g) { case 1: break; default: g = 0.5; }
const { a, ...others } = { a: 1, b: 2 }; [l, ...[c]] = [1, 2];
eval('1');
"#;

#[test]
fn a_script_with_every_kind_of_function_round_trips() {
    let code = compile_script(SCRIPT, "all.js").unwrap();
    assert_round_trips(&code);
}

#[test]
fn named_operand_and_fused_branch_instructions_round_trip_and_are_verified() {
    // TASK-100.18: every operand form and both branch polarities.
    let src = "function f(a, b, o) {
        var s = a * b + (a - 1) + (o.x * 2) + (3 * a);
        if (a < b && !(a >= 7) || o.x > 1) s = s % 1000003;
        while (b > a) { b--; }
        do { a++; } while (a <= 5);
        do { b++; } while (o.x < 3);
        do { s--; } while (s < b);
        do { s--; } while (a < o.x);
        for (var i = 0; i < b; i++) { s = s + i; }
        return a == b ? s : a === o.x ? -s : 0;
    }";
    let code = compile_script(src, "ops.js").unwrap();
    let f = &code.functions[0];
    let shapes: [(&str, fn(&Instr) -> bool); 12] = [
        ("BinRR", |i| matches!(i, Instr::BinRR { .. })),
        ("BinRI", |i| matches!(i, Instr::BinRI { .. })),
        ("BinAR", |i| matches!(i, Instr::BinAR { .. })),
        ("BinAI", |i| matches!(i, Instr::BinAI { .. })),
        ("JumpIfCmpRR", |i| matches!(i, Instr::JumpIfCmpRR { .. })),
        ("JumpIfNotCmpRR", |i| matches!(i, Instr::JumpIfNotCmpRR { .. })),
        ("JumpIfCmpRI", |i| matches!(i, Instr::JumpIfCmpRI { .. })),
        ("JumpIfNotCmpRI", |i| matches!(i, Instr::JumpIfNotCmpRI { .. })),
        ("JumpIfCmpRA", |i| matches!(i, Instr::JumpIfCmpRA { .. })),
        ("JumpIfNotCmpRA", |i| matches!(i, Instr::JumpIfNotCmpRA { .. })),
        ("JumpIfCmpAI", |i| matches!(i, Instr::JumpIfCmpAI { .. })),
        ("JumpIfNotCmpAI", |i| matches!(i, Instr::JumpIfNotCmpAI { .. })),
    ];
    for (name, shape) in shapes {
        assert!(f.instrs.iter().any(shape), "no {name} in {:#?}", f.instrs);
    }
    assert_round_trips(&code);

    // A fused branch names a comparison; an arithmetic operator there is
    // refused, as are registers and jump targets outside the block.
    let cases: [(fn(&mut Instr), &str); 3] = [
        (
            |i| {
                if let Instr::JumpIfNotCmpRR { op, .. } = i {
                    *op = BinOp::Add
                }
            },
            "comparison",
        ),
        (
            |i| {
                if let Instr::BinRI { a, .. } = i {
                    *a = 999
                }
            },
            "register",
        ),
        (
            |i| {
                if let Instr::JumpIfCmpAI { offset, .. } = i {
                    *offset = 10_000
                }
            },
            "jump",
        ),
    ];
    for (tamper, what) in cases {
        let code = compile_script(src, "ops.js").unwrap();
        let mut owned = Rc::try_unwrap(code).expect("sole owner");
        let f = Rc::get_mut(&mut owned.functions[0]).expect("sole owner");
        let before = f.instrs.clone();
        f.instrs.iter_mut().for_each(tamper);
        assert_ne!(before, f.instrs, "{what}: nothing was tampered");
        match decode(&encode(&owned)) {
            Err(DecodeError::Invalid(m)) => assert!(m.contains(what), "{m}"),
            other => panic!("expected a verification error for {what}, got {other:?}"),
        }
    }
}

#[test]
fn a_module_with_imports_exports_and_top_level_await_round_trips() {
    let src = "import d, { x as y } from './dep.js' with { type: 'json' };\n\
               import * as ns from 'ns';\n\
               export * from 'star'; export * as all from 'star2';\n\
               export { y as z }; export { w } from 'w';\n\
               export default class {}\n\
               export const v = await Promise.resolve(import.meta.url);";
    let code = compile_module(src, "app/m.js").unwrap();
    assert_round_trips(&code);
}

#[test]
fn the_source_text_is_stored_once_and_shared_by_nested_functions() {
    let body = "x".repeat(10_000);
    let src = format!("function a() {{ function b() {{ return '{body}'; }} }}");
    let code = compile_script(&src, "big.js").unwrap();
    let bytes = encode(&code);
    // One copy of the source (UTF-8) and of the string constant (UTF-16);
    // a copy of the source per function would exceed this.
    assert!(bytes.len() < 3 * src.len() + 2_000, "{} bytes", bytes.len());
    let back = decode(&bytes).unwrap();
    let inner = &back.functions[0];
    assert!(std::rc::Rc::ptr_eq(&back.source, &inner.source));
    assert_eq!(&*back.source.text, src.as_str());
}

#[test]
fn inline_caches_start_empty_after_decode() {
    let code = compile_script("var o = { a: 1 }; o.a; o.a = 2;", "ic.js").unwrap();
    // Pretend the VM filled the feedback before the tree was saved.
    let fb = code.feedback(1).unwrap();
    fb.ic[0].entries[0].set(agnijs_js_compiler::IcEntry {
        kind: agnijs_js_compiler::IcEntry::KIND_OWN_DATA,
        flags: 0,
        _reserved: [0; 2],
        shape: 7,
        proto: 1,
        holder: 2,
        slot: 3,
        epoch: 4,
    });
    fb.name_keys[0].set(99);
    let back = decode(&encode(&code)).unwrap();
    assert_eq!(back.ic_count, code.ic_count);
    // Pre-interned names and caches are run-time state: a loaded block
    // starts with none, and whichever VM runs it first allocates them.
    assert!(back.feedback.get().is_none());
    let fb = back.feedback(2).unwrap();
    assert!(fb
        .ic
        .iter()
        .all(|s| s.state() == agnijs_js_compiler::IcState::Empty));
    assert!(fb.name_keys.iter().all(|k| k.get() == 0));
    assert!(back.feedback(1).is_none(), "feedback belongs to its first VM");
}

/// The compiler's debug info for `code`, taken out of its section.
fn debug_of(code: CodeBlock) -> (CodeBlock, DebugInfo) {
    let mut code = code;
    let info = std::mem::take(&mut code.debug).into_info().expect("compiled debug info");
    (code, info)
}

#[test]
fn debug_info_round_trips_including_backward_offsets_and_multi_byte_values() {
    let code = compile_script("let a = 1;\nlet b = a + 1;\nb;", "d.js").unwrap();
    let (mut owned, mut debug) = debug_of(Rc::try_unwrap(code).expect("sole owner"));
    assert!(debug.statements.len() >= 3);
    // A statement that starts before the one ahead of it (a negative
    // delta), and a position whose delta takes three LEB128 bytes.
    let len = owned.instrs.len() as u32;
    debug.statements[1].offset = 0;
    owned.source = Rc::new(agnijs_js_compiler::SourceText {
        name: owned.source.name.clone(),
        text: format!("{}{}", owned.source.text, " ".repeat(300_000)).into(),
    });
    let last = debug.statements.len() - 1;
    debug.statements[last].offset = 299_999;
    debug.locals.push(agnijs_js_compiler::LocalVar {
        name: "late".into(),
        reg: 0,
        start: len - 1,
        end: len,
    });
    owned.debug = DebugSection::from(debug.clone());
    let back = decode(&encode(&owned)).unwrap();
    let got = back.debug.get().unwrap();
    assert_eq!(got.statements, debug.statements);
    assert_eq!(format!("{:?}", got.locals), format!("{:?}", debug.locals));
}

/// Every pool whose count is in the payload, recursively.
fn check_exact_capacity(code: &CodeBlock, path: &str) {
    fn exact<T>(v: &Vec<T>, what: &str, path: &str) {
        assert_eq!(v.capacity(), v.len(), "{path}: {what} over-allocated");
    }
    exact(&code.instrs, "instrs", path);
    exact(&code.offsets, "offsets", path);
    exact(&code.constants, "constants", path);
    exact(&code.names, "names", path);
    exact(&code.param_names, "param_names", path);
    exact(&code.functions, "functions", path);
    exact(&code.scopes, "scopes", path);
    for (i, s) in code.scopes.iter().enumerate() {
        exact(&s.bindings, &format!("scopes[{i}].bindings"), path);
    }
    exact(&code.handlers, "handlers", path);
    if let Some(d) = &code.declarations {
        exact(&d.var_names, "declarations.var_names", path);
        exact(&d.functions, "declarations.functions", path);
        exact(&d.lexical, "declarations.lexical", path);
        exact(&d.annex_b_functions, "declarations.annex_b_functions", path);
    }
    if let Some(m) = &code.module {
        exact(&m.requested, "module.requested", path);
        for (i, q) in m.requested.iter().enumerate() {
            exact(&q.attributes, &format!("module.requested[{i}].attributes"), path);
        }
        exact(&m.imports, "module.imports", path);
        exact(&m.local_exports, "module.local_exports", path);
        exact(&m.indirect_exports, "module.indirect_exports", path);
        exact(&m.star_exports, "module.star_exports", path);
    }
    for k in &code.constants {
        if let agnijs_js_compiler::Constant::Template(t) = k {
            exact(&t.cooked, "template.cooked", path);
            exact(&t.raw, "template.raw", path);
        }
    }
    let d = code.debug.get().unwrap();
    exact(&d.statements, "debug.statements", path);
    exact(&d.locals, "debug.locals", path);
    for (i, f) in code.functions.iter().enumerate() {
        check_exact_capacity(f, &format!("{path}/{}[{i}]", f.name));
    }
}

#[test]
fn decoding_allocates_every_pool_at_its_final_size() {
    for (src, name, module) in [
        (SCRIPT, "all.js", false),
        (
            "import d, { x as y } from './dep.js' with { type: 'json' };\n\
             export * from 'star'; export { y as z }; export { w } from 'w';\n\
             export default function f(a, b) { let c = a; { let e = b; c += e; } return c; }",
            "m.js",
            true,
        ),
    ] {
        let code = if module {
            compile_module(src, name).unwrap()
        } else {
            compile_script(src, name).unwrap()
        };
        let back = decode(&encode(&code)).unwrap();
        check_exact_capacity(&back, name);
    }
}

#[test]
fn debug_info_is_decoded_on_first_use_not_at_load() {
    let code = compile_script("let a = 1;\nfunction f(x) { let y = x; return y; }\nf(a);", "lazy.js").unwrap();
    assert!(code.debug.is_decoded(), "the compiler's own info is already decoded");
    let back = decode(&encode(&code)).unwrap();
    assert!(!back.debug.is_decoded(), "loading must not decode the debug section");
    assert!(!back.functions[0].debug.is_decoded());
    // Stepping asks whether a pc starts a statement; that is the first use.
    let first = code.debug.get().unwrap().statements[0].pc as usize;
    assert!(back.debug.is_statement(first));
    assert!(back.debug.is_decoded());
    assert!(!back.functions[0].debug.is_decoded(), "each block decodes on its own");
    let inner = back.functions[0].debug.get().unwrap();
    assert_eq!(inner.locals.iter().map(|l| &*l.name).collect::<Vec<_>>(), ["x", "y"]);
    assert_eq!(
        back.debug.get().unwrap().statements,
        code.debug.get().unwrap().statements
    );
}

#[test]
fn a_malformed_debug_section_loads_and_is_refused_at_first_use() {
    let tampers: [(&str, fn(&mut DebugInfo, &CodeBlock)); 5] = [
        ("out of order", |d, _| d.statements.swap(0, 1)),
        ("past the block", |d, c| d.statements[0].pc = c.instrs.len() as u32),
        ("past the text", |d, c| d.statements[0].offset = c.source.text.len() as u32 + 1),
        ("register", |d, c| {
            d.locals.push(agnijs_js_compiler::LocalVar {
                name: "bad".into(),
                reg: c.register_count,
                start: 0,
                end: 1,
            })
        }),
        ("range", |d, c| {
            d.locals.push(agnijs_js_compiler::LocalVar {
                name: "bad".into(),
                reg: 0,
                start: 0,
                end: c.instrs.len() as u32 + 1,
            })
        }),
    ];
    for (what, tamper) in tampers {
        let code = compile_script("let a = 1;\nlet b = a + 1;\nb;", "bad.js").unwrap();
        let (mut owned, mut debug) = debug_of(Rc::try_unwrap(code).expect("sole owner"));
        tamper(&mut debug, &owned);
        owned.debug = DebugSection::from(debug);
        let back = decode(&encode(&owned)).unwrap_or_else(|e| panic!("{what}: load refused: {e}"));
        assert_eq!(back.instrs, owned.instrs, "{what}: the code itself is intact");
        assert!(
            matches!(back.debug.get(), Err(DecodeError::Invalid(_))),
            "{what}: {:?}",
            back.debug.get()
        );
        // The verdict is kept, and the debugger sees no statements or locals.
        assert!(back.debug.get().is_err(), "{what}");
        assert!(!back.debug.is_statement(0), "{what}");
        assert!(back.debug.statements().is_empty() && back.debug.locals().is_empty(), "{what}");
    }
}

#[test]
fn truncated_input_is_refused_at_every_length() {
    let code = compile_script(SCRIPT, "all.js").unwrap();
    let bytes = encode(&code);
    for len in 0..bytes.len() {
        assert!(
            decode(&bytes[..len]).is_err(),
            "a prefix of {len} of {} bytes decoded",
            bytes.len()
        );
    }
}

#[test]
fn corrupted_bytes_never_panic() {
    let code = compile_script(SCRIPT, "all.js").unwrap();
    let bytes = encode(&code);
    // Flip each byte in turn (stepping to keep the test fast): decoding
    // either fails with an error or yields a tree that passed verification.
    let step = (bytes.len() / 4000).max(1);
    for i in (0..bytes.len()).step_by(step) {
        for mask in [0x01u8, 0x80, 0xFF] {
            let mut bad = bytes.clone();
            bad[i] ^= mask;
            let _ = decode(&bad);
        }
    }
}

#[test]
fn operands_outside_their_pools_are_rejected_by_verification() {
    let code = compile_script("var s = 'k'; s;", "v.js").unwrap();
    let mut bytes = encode(&code);
    // Trailing bytes are not part of any valid tree.
    bytes.push(0);
    assert!(matches!(decode(&bytes), Err(DecodeError::TrailingBytes)));

    // Encoding writes whatever it is given; decoding verifies.
    let cases: [(&str, fn(&mut Instr), &str); 4] = [
        (
            "'only'",
            |i| {
                if let Instr::LdaConst(c) = i {
                    *c = 99
                }
            },
            "constant",
        ),
        (
            "var r; r;",
            |i| {
                if let Instr::Ldar(r) = i {
                    *r = 999
                }
            },
            "register",
        ),
        (
            "while (x) {}",
            |i| {
                if let Instr::Jump(o) = i {
                    *o = 10_000
                }
            },
            "jump",
        ),
        (
            "(function () {})",
            |i| {
                if let Instr::CreateClosure(f) = i {
                    *f = 7
                }
            },
            "function",
        ),
    ];
    for (src, tamper, what) in cases {
        let code = compile_script(src, "v.js").unwrap();
        let mut owned = Rc::try_unwrap(code).expect("sole owner");
        let before = owned.instrs.clone();
        owned.instrs.iter_mut().for_each(tamper);
        assert_ne!(before, owned.instrs, "{src}: nothing was tampered");
        match decode(&encode(&owned)) {
            Err(DecodeError::Invalid(m)) => assert!(m.contains(what), "{src}: {m}"),
            other => panic!("{src}: expected a verification error, got {other:?}"),
        }
    }
}
