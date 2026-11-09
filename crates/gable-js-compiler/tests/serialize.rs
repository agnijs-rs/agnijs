//! Serialised bytecode: a compiled code tree survives encode and decode
//! unchanged, and bad input is refused with an error instead of a panic.

use gable_js_compiler::serialize::{decode, encode, DecodeError};
use gable_js_compiler::{compile_module, compile_script, CodeBlock, Instr};
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
    // Pretend the VM filled a cache before the tree was saved.
    code.ic[0].set(gable_js_compiler::IcEntry {
        shape: 7,
        holder: 1,
        holder_shape: 2,
        slot: 3,
    });
    let back = decode(&encode(&code)).unwrap();
    assert_eq!(back.ic.len(), code.ic.len());
    assert!(back
        .ic
        .iter()
        .all(|c| c.get() == gable_js_compiler::IcEntry::EMPTY));
}

#[test]
fn debug_info_round_trips_including_backward_offsets_and_multi_byte_values() {
    let code = compile_script("let a = 1;\nlet b = a + 1;\nb;", "d.js").unwrap();
    let mut owned = Rc::try_unwrap(code).expect("sole owner");
    assert!(owned.debug.statements.len() >= 3);
    // A statement that starts before the one ahead of it (a negative
    // delta), and a position whose delta takes three LEB128 bytes.
    let len = owned.instrs.len() as u32;
    owned.debug.statements[1].offset = 0;
    owned.source = Rc::new(gable_js_compiler::SourceText {
        name: owned.source.name.clone(),
        text: format!("{}{}", owned.source.text, " ".repeat(300_000)).into(),
    });
    let last = owned.debug.statements.len() - 1;
    owned.debug.statements[last].offset = 299_999;
    owned.debug.locals.push(gable_js_compiler::LocalVar {
        name: "late".into(),
        reg: 0,
        start: len - 1,
        end: len,
    });
    let back = decode(&encode(&owned)).unwrap();
    assert_eq!(back.debug.statements, owned.debug.statements);
    assert_eq!(format!("{:?}", back.debug.locals), format!("{:?}", owned.debug.locals));
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
