//! What the code generator knows about a block's registers before it
//! generates anything (TASK-101.3.1): which ones its runtime paths read,
//! which ones its loops use (the cache), which are live where, and which
//! a runtime may usefully speculate to hold numbers. Plain bytecode
//! analysis, so the runtime can ask the last question without a host
//! code generator.

use std::collections::BTreeMap;

use agnijs_js_compiler::bytecode::{CodeBlock, Constant, Instr};

use crate::Speculation;

/// How many registers a block keeps in machine state at most.
pub(crate) const MAX_CACHED: usize = 8;

/// The branch offset of an instruction that may branch, for the
/// instructions whose taken edge can come back from the runtime.
pub(crate) fn branch_offset(instr: &Instr) -> Option<i32> {
    Some(match *instr {
        Instr::Jump(o)
        | Instr::JumpIfTrue(o)
        | Instr::JumpIfFalse(o)
        | Instr::JumpIfNullish(o)
        | Instr::JumpIfNotNullish(o)
        | Instr::JumpIfUndefined(o)
        | Instr::JumpIfNotUndefined(o)
        | Instr::JumpIfEmpty(o)
        | Instr::JumpIfNotEmpty(o)
        | Instr::JumpIfObject(o) => o,
        Instr::JumpIfStrictEq { offset, .. }
        | Instr::JumpIfCmpRR { offset, .. }
        | Instr::JumpIfNotCmpRR { offset, .. }
        | Instr::JumpIfCmpRI { offset, .. }
        | Instr::JumpIfNotCmpRI { offset, .. }
        | Instr::JumpIfCmpRA { offset, .. }
        | Instr::JumpIfNotCmpRA { offset, .. }
        | Instr::JumpIfCmpAI { offset, .. }
        | Instr::JumpIfNotCmpAI { offset, .. } => offset,
        Instr::IteratorStep { done, .. }
        | Instr::IteratorResult { done }
        | Instr::ForInNext { done, .. } => done,
        _ => return None,
    })
}

/// The registers an instruction's runtime path reads: a known few, or
/// (a generic instruction, whose semantics the code generator does not
/// decode) possibly any, and then possibly written too.
pub(crate) enum Access {
    Known(Vec<u16>),
    Any,
}

pub(crate) fn access(instr: &Instr) -> Access {
    use Instr::*;
    Access::Known(match *instr {
        Ldar(r) | Add(r) | Sub(r) | Mul(r) | Div(r) | Mod(r) | Exp(r) | Shl(r) | Shr(r)
        | UShr(r) | BitAnd(r) | BitOr(r) | BitXor(r) | Eq(r) | Ne(r) | StrictEq(r)
        | StrictNe(r) | Lt(r) | Le(r) | Gt(r) | Ge(r) => vec![r],
        Mov { src, .. } => vec![src],
        BinRR { a, b, .. } | JumpIfCmpRR { a, b, .. } | JumpIfNotCmpRR { a, b, .. } => vec![a, b],
        BinRI { a, .. }
        | JumpIfCmpRI { a, .. }
        | JumpIfNotCmpRI { a, .. }
        | JumpIfCmpRA { a, .. }
        | JumpIfNotCmpRA { a, .. } => vec![a],
        BinAR { b, .. } => vec![b],
        JumpIfStrictEq { reg, .. } => vec![reg],
        GetNamed { obj, .. } | SetNamed { obj, .. } => vec![obj],
        GetKeyed { obj, key } | SetKeyed { obj, key, .. } => vec![obj, key],
        Call {
            callee,
            this,
            args,
            argc,
        } => {
            let mut v = vec![callee, this];
            v.extend((0..argc).map(|i| args + i));
            v
        }
        Star(_) | BinAI { .. } | JumpIfCmpAI { .. } | JumpIfNotCmpAI { .. } | Inc | Dec
        | ToNumeric | CheckTdz(_) | LdaUndefined | LdaNull | LdaTrue | LdaFalse | LdaEmpty
        | LdaInt(_) | LdaConst(_) | LdaThis | Nop | Return | Jump(_) | JumpIfTrue(_)
        | JumpIfFalse(_) | JumpIfNullish(_) | JumpIfNotNullish(_) | JumpIfUndefined(_)
        | JumpIfNotUndefined(_) | JumpIfEmpty(_) | JumpIfNotEmpty(_) | JumpIfObject(_)
        | LdaGlobal(_) | LdaEnv { .. } | StaEnv { .. } | LdaCallee => vec![],
        _ => return Access::Any,
    })
}

/// The registers an instruction's inline number fast path reads: what a
/// number speculation would spare a tag test on.
fn number_operands(instr: &Instr) -> Vec<u16> {
    use Instr::*;
    match *instr {
        Add(r) | Sub(r) | Mul(r) | Div(r) | Mod(r) | Exp(r) | Shl(r) | Shr(r) | UShr(r)
        | BitAnd(r) | BitOr(r) | BitXor(r) | Eq(r) | Ne(r) | StrictEq(r) | StrictNe(r)
        | Lt(r) | Le(r) | Gt(r) | Ge(r) => vec![r],
        BinRR { a, b, .. } | JumpIfCmpRR { a, b, .. } | JumpIfNotCmpRR { a, b, .. } => vec![a, b],
        BinRI { a, .. }
        | JumpIfCmpRI { a, .. }
        | JumpIfNotCmpRI { a, .. }
        | JumpIfCmpRA { a, .. }
        | JumpIfNotCmpRA { a, .. } => vec![a],
        BinAR { b, .. } => vec![b],
        _ => vec![],
    }
}

/// The register an instruction's inline expansion writes.
pub(crate) fn written(instr: &Instr) -> Option<u16> {
    match *instr {
        Instr::Star(r) | Instr::Mov { dst: r, .. } => Some(r),
        _ => None,
    }
}

/// How deep in loops each instruction is (a back-edge's range is a loop).
pub(crate) fn loop_depth(code: &CodeBlock) -> Vec<u8> {
    let len = code.instrs.len();
    let mut depth = vec![0u8; len];
    for (pc, instr) in code.instrs.iter().enumerate() {
        if let Some(off) = branch_offset(instr) {
            if off <= 0 {
                let t = (pc as i64 + off as i64).max(0) as usize;
                for d in &mut depth[t..=pc] {
                    *d = d.saturating_add(1);
                }
            }
        }
    }
    depth
}

/// The weight of a use at loop depth `depth`.
fn weight(depth: u8) -> u64 {
    match depth {
        0 => 0,
        1 => 8,
        _ => 64,
    }
}

/// The registers to keep in machine state: those loops read or write by
/// inline-expanded instructions, the most used first, the speculated ones
/// before the rest. A block without loops caches nothing: its call outs
/// would pay for the caching and nothing would gain.
pub(crate) fn choose_cache(code: &CodeBlock, spec: &Speculation) -> Vec<(u16, bool)> {
    let depth = loop_depth(code);
    let mut score: BTreeMap<u16, u64> = BTreeMap::new();
    for (pc, instr) in code.instrs.iter().enumerate() {
        let w = weight(depth[pc]);
        if w == 0 {
            continue;
        }
        let mut refs = match access(instr) {
            Access::Known(r) => r,
            Access::Any => Vec::new(),
        };
        refs.extend(written(instr));
        if matches!(instr, Instr::Call { .. }) {
            // Read by the runtime, not inline: caching them helps nothing.
            refs.clear();
        }
        for r in refs {
            *score.entry(r).or_default() += w;
        }
    }
    let speculated = |r: u16| r < 64 && spec.numbers & (1 << r) != 0;
    let mut regs: Vec<(u16, u64)> = score.into_iter().collect();
    regs.sort_by(|a, b| {
        speculated(b.0)
            .cmp(&speculated(a.0))
            .then(b.1.cmp(&a.1))
            .then(a.0.cmp(&b.0))
    });
    regs.truncate(MAX_CACHED);
    regs.into_iter().map(|(r, _)| (r, speculated(r))).collect()
}

/// The registers (below 64) a runtime may usefully speculate to hold
/// numbers ([`Speculation::numbers`]): those a loop's inline arithmetic or
/// comparison reads, which compiled code would then read without a tag
/// test. The runtime keeps those that hold a number in the frame it
/// compiles for, less any whose speculation failed before.
pub fn number_candidates(code: &CodeBlock) -> u64 {
    let depth = loop_depth(code);
    let mut mask = 0u64;
    for (pc, instr) in code.instrs.iter().enumerate() {
        if depth[pc] == 0 {
            continue;
        }
        for r in number_operands(instr) {
            if r < 64 {
                mask |= 1 << r;
            }
        }
    }
    mask
}

/// Whether the instruction never calls out of compiled code: its
/// expansion is machine code alone, with no generic path behind a guard.
/// (A jump among these still polls on a back-edge.)
pub(crate) fn always_inline(code: &CodeBlock, instr: &Instr) -> bool {
    match *instr {
        Instr::LdaConst(c) => matches!(code.constants.get(c as usize), Some(Constant::Number(_))),
        Instr::LdaUndefined
        | Instr::LdaNull
        | Instr::LdaTrue
        | Instr::LdaFalse
        | Instr::LdaEmpty
        | Instr::LdaInt(_)
        | Instr::Ldar(_)
        | Instr::Star(_)
        | Instr::Mov { .. }
        | Instr::Nop
        | Instr::LdaThis
        | Instr::Jump(_)
        | Instr::JumpIfNullish(_)
        | Instr::JumpIfNotNullish(_)
        | Instr::JumpIfUndefined(_)
        | Instr::JumpIfNotUndefined(_)
        | Instr::JumpIfEmpty(_)
        | Instr::JumpIfNotEmpty(_)
        | Instr::JumpIfObject(_)
        | Instr::Return => true,
        _ => false,
    }
}

/// Whether running the instruction at `pc` may end in a throw that one of
/// the block's own handlers catches: it reaches the runtime, by calling out
/// or (a back-edge) by polling. Only what [`always_inline`] names and does
/// not branch backwards cannot; every other instruction is assumed to.
fn may_throw(code: &CodeBlock, instr: &Instr) -> bool {
    !always_inline(code, instr) || branch_offset(instr).is_some_and(|off| off <= 0)
}

/// The pcs an instruction may continue at by normal control flow.
fn successors(code: &CodeBlock, pc: usize) -> Vec<usize> {
    let instr = &code.instrs[pc];
    match *instr {
        Instr::Return => vec![],
        Instr::Jump(off) => vec![(pc as i64 + off as i64) as usize],
        _ => {
            let mut s = vec![pc + 1];
            if let Some(off) = branch_offset(instr) {
                s.push((pc as i64 + off as i64) as usize);
            }
            s
        }
    }
}

/// For each pc, the registers (the bit `bit` gives each, `all` being every
/// bit) that may be read before written from there, by any path: normal
/// control flow, and a throw from an instruction inside a handler's range
/// to that handler (TASK-103.34). A generic instruction may read any
/// register and is assumed to write none.
///
/// The edge to a handler leaves from before the instruction: a throw
/// happens before the instruction's own write, so that write does not make
/// the register dead for the handler. Every handler whose range holds the
/// instruction counts, not only the innermost, which is the one a throw
/// goes to: more than is needed, never less.
fn live_sets(code: &CodeBlock, bit: &dyn Fn(u16) -> u64, all: u64) -> Vec<u64> {
    let len = code.instrs.len();
    let throws: Vec<bool> = code.instrs.iter().map(|i| may_throw(code, i)).collect();
    let mut live_in = vec![0u64; len + 1];
    loop {
        let mut changed = false;
        for pc in (0..len).rev() {
            let instr = &code.instrs[pc];
            let new = match access(instr) {
                Access::Any => all,
                Access::Known(reads) => {
                    let mut out = 0;
                    for s in successors(code, pc) {
                        out |= live_in.get(s).copied().unwrap_or(0);
                    }
                    let mut caught = 0;
                    if throws[pc] {
                        for h in &code.handlers {
                            if h.start as usize <= pc && pc < h.end as usize {
                                caught |= live_in.get(h.target as usize).copied().unwrap_or(0);
                            }
                        }
                    }
                    let uses = reads.iter().fold(0, |m, &r| m | bit(r));
                    let def = written(instr).map_or(0, bit);
                    uses | (out & !def) | caught
                }
            };
            if new != live_in[pc] {
                live_in[pc] = new;
                changed = true;
            }
        }
        if !changed {
            return live_in;
        }
    }
}

/// For each pc, the cached registers (a bit per cache index) that may be
/// read before written from there ([`live_sets`]): what must hold its value
/// when compiled code is entered or resumed at that pc.
pub(crate) fn liveness(code: &CodeBlock, index: &[Option<u8>], n: usize) -> Vec<u16> {
    let all: u64 = if n >= 16 {
        u64::from(u16::MAX)
    } else {
        (1u64 << n) - 1
    };
    let bit = |r: u16| -> u64 {
        index
            .get(r as usize)
            .copied()
            .flatten()
            .map_or(0, |i| 1 << i)
    };
    live_sets(code, &bit, all)
        .into_iter()
        .map(|m| m as u16)
        .collect()
}

/// The registers (below 64, a bit each) that may be read before written
/// when `code` runs from `pc`, a handler's reads after a throw included:
/// what holds a value that matters there. A runtime that guesses a register
/// "holds a number or nothing yet" ([`Speculation::numbers`]) may count an
/// unassigned one only when its bit is clear: it is written before anything
/// reads it. A `pc` outside the block has every register live.
pub fn live_registers(code: &CodeBlock, pc: usize) -> u64 {
    let bit = |r: u16| -> u64 {
        if r < 64 {
            1 << r
        } else {
            0
        }
    };
    let live = live_sets(code, &bit, u64::MAX);
    if pc < code.instrs.len() {
        live[pc]
    } else {
        u64::MAX
    }
}

#[cfg(test)]
mod tests {
    use agnijs_js_compiler::compile_script;

    use super::*;

    fn function(src: &str) -> std::rc::Rc<CodeBlock> {
        compile_script(src, "test.js").expect("compiles").functions[0].clone()
    }

    #[test]
    fn candidates_are_the_registers_loop_arithmetic_reads() {
        let code = function("function f(a, n) { var s = 0; for (var i = 0; i < n; i++) { s = s + a; } return s; }");
        let c = number_candidates(&code);
        // `s` and `a` (the add), `i` and `n` (the compare): four of them.
        assert_eq!(c.count_ones(), 4, "{c:#b}");
        let straight = function("function g(a, b) { return a + b; }");
        assert_eq!(number_candidates(&straight), 0, "no loop: nothing to gain");
    }

    #[test]
    fn a_block_without_loops_caches_nothing() {
        let code = function("function g(a, b) { var c = a + b; return c * 2; }");
        assert!(choose_cache(&code, &Speculation::NONE).is_empty());
    }

    #[test]
    fn a_register_written_before_read_is_dead_at_entry() {
        let code = function("function f(n) { var s = 0; for (var i = 0; i < n; i++) { s = s + i; } return s; }");
        let spec = Speculation { numbers: number_candidates(&code) };
        let cache = choose_cache(&code, &spec);
        let mut index = vec![None; code.register_count as usize];
        for (i, &(r, _)) in cache.iter().enumerate() {
            index[r as usize] = Some(i as u8);
        }
        let live = liveness(&code, &index, cache.len());
        // At the function's start no local has been assigned: only the
        // parameter's register (copied in first) may be live.
        let s = code
            .instrs
            .windows(2)
            .find_map(|w| match (w[0], w[1]) {
                (Instr::LdaInt(_), Instr::Star(r)) => Some(r),
                _ => None,
            })
            .unwrap();
        let bit = 1u16 << index[s as usize].unwrap();
        assert_eq!(live[0] & bit, 0, "`s` is dead at pc 0");
        // At the loop's compare, `s` is live (read by the add after it).
        let cmp = code
            .instrs
            .iter()
            .position(|i| matches!(i, Instr::JumpIfNotCmpRI { .. } | Instr::JumpIfCmpRI { .. } | Instr::JumpIfNotCmpRR { .. } | Instr::JumpIfCmpRR { .. }))
            .unwrap();
        assert_ne!(live[cmp] & bit, 0, "`s` is live at the loop head");
    }

    /// The liveness of `code` with every candidate guessed, and the cache
    /// index of each register.
    fn live_guessing(code: &CodeBlock) -> (Vec<u16>, Vec<Option<u8>>) {
        let spec = Speculation {
            numbers: number_candidates(code),
        };
        let cache = choose_cache(code, &spec);
        let mut index = vec![None; code.register_count as usize];
        for (i, &(r, _)) in cache.iter().enumerate() {
            index[r as usize] = Some(i as u8);
        }
        (liveness(code, &index, cache.len()), index)
    }

    // TASK-103.34: the analysis followed normal control flow only, so a
    // local every normal path writes before reading was dead even where a
    // throw could take it, unwritten, to a handler that reads it.
    #[test]
    fn a_register_a_handler_reads_is_live_where_a_throw_can_come_from() {
        let code = function(
            "function f(n, o) { var x; var y = 0; try { for (var i = 0; i < n; i++) { o.p; x = i; y = x + 1; } } catch (e) { return x; } return y; }",
        );
        let (live, index) = live_guessing(&code);
        let h = code.handlers[0];
        let in_range = |pc: usize| h.start as usize <= pc && pc < h.end as usize;
        // `x`: the first cached register the handler loads.
        let x = (h.target as usize..code.instrs.len())
            .find_map(|pc| match code.instrs[pc] {
                Instr::Ldar(r) if index[r as usize].is_some() => Some(r),
                _ => None,
            })
            .expect("the handler reads a cached register");
        let bit = 1u16 << index[x as usize].unwrap();
        let throws = code
            .instrs
            .iter()
            .position(|i| matches!(i, Instr::GetNamed { .. }))
            .expect("`o.p`");
        assert!(in_range(throws));
        let write = (throws..code.instrs.len())
            .find(|&pc| written(&code.instrs[pc]) == Some(x))
            .expect("`x = i` after the load");
        assert!(in_range(write));
        // On the normal path `x` is written before it is read (the
        // instruction after the write does not need it from before)...
        assert_eq!(live[write] & bit, 0, "`x` is dead at its own assignment");
        // ...but the load can throw to the handler, which reads it: live
        // there, and so at every entry before it, the function's start
        // included.
        assert_ne!(
            live[throws] & bit,
            0,
            "`x` is live at the instruction that can throw"
        );
        assert_ne!(live[0] & bit, 0, "`x` is live at pc 0");
    }

    #[test]
    fn an_instruction_that_cannot_throw_has_no_edge_to_its_handler() {
        // `i` and `y` are assigned inside the `try` before anything in it
        // can throw: the handler never sees them unassigned, so neither is
        // live at the function's start, though the handler reads both.
        let code = function(
            "function f(n, o) { try { var y = 0; for (var i = 0; i < n; i++) { o.p; y = y + i; } } catch (e) { return y + i; } return y; }",
        );
        let (live, index) = live_guessing(&code);
        let h = code.handlers[0];
        let mut assigned = Vec::new();
        for pc in h.start as usize..h.end as usize {
            if matches!(code.instrs[pc], Instr::GetNamed { .. }) {
                break;
            }
            assigned.extend(written(&code.instrs[pc]));
        }
        let cached: Vec<u16> = assigned
            .into_iter()
            .filter(|&r| index[r as usize].is_some())
            .collect();
        assert!(cached.len() >= 2, "`y` and `i`: {cached:?}");
        for r in cached {
            let bit = 1u16 << index[r as usize].unwrap();
            assert_eq!(live[0] & bit, 0, "register {r} is dead at pc 0");
        }
    }
}
