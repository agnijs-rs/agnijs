//! The backtracking matcher (TASK-100.31): every pattern runs here unless
//! the routing rule sends it to the linear-time matcher.
//!
//! The parse tree compiles to a flat program run with one explicit stack
//! (no recursion, so no pattern or input can overflow the native stack).
//! The stack holds choice points and the old values of the registers
//! (captures, loop counters) changed since; backtracking pops it, undoing
//! register writes until it reaches a choice point to resume.
//!
//! ECMAScript semantics:
//!
//! - **Priority.** A split tries its first branch and resumes the second
//!   on backtracking; greedy loops try another iteration first, lazy ones
//!   the continuation.
//! - **Empty iterations.** An iteration past the minimum that consumes
//!   nothing fails (RepeatMatcher's continuation), which also makes every
//!   loop terminate: `/(?:(_|)+){2}K/` cannot hang.
//! - **Capture reset.** Each iteration starts with the captures inside the
//!   quantified atom unset.
//! - **Captures** are written when their group completes, so a
//!   backreference inside the group sees the old value.
//! - **Lookaround** is atomic: a positive lookaround that matched drops its
//!   choice points but keeps its register undo records (its captures stay
//!   until something before it is backtracked); a negative one unwinds.
//!   Lookbehind bodies are compiled to match right to left.
//!
//! Speed: a quantified single character class runs as one instruction
//! that scans the run and backs off one character at a time; when nothing
//! the class matches can begin the continuation, the loop is possessive
//! and pushes no choice point at all. The first characters a match can
//! begin with let the search skip start positions.

use std::collections::HashMap;

use crate::charset::{CharSet, SetMatcher};
use crate::parse::{Assertion, Node, Pattern};
use crate::unicode;

pub(crate) const NONE: usize = usize::MAX;
const NO_SET: u32 = u32::MAX;
pub(crate) const INFINITE: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Inst {
    Char { c: u32, back: bool },
    Set { set: u32, back: bool },
    /// The literal code units `lits[start..start + len]`.
    Str { start: u32, len: u32, back: bool },
    /// Try `pc + 1`, then `alt`.
    Split { alt: u32 },
    Jmp(u32),
    /// Record where group `g` began (or, matching backwards, ended).
    Open(u32),
    /// Group `g` is complete: write its capture.
    Close(u32),
    Start { multiline: bool },
    End { multiline: bool },
    Word { negate: bool, set: u32 },
    /// Zero loop `lp`'s iteration count.
    LoopInit(u32),
    /// The loop decides: another iteration (`pc + 1`) or `exit`.
    LoopHead { lp: u32, exit: u32 },
    /// An iteration begins: count it, note where, unset its captures.
    LoopBegin(u32),
    /// An iteration ended: fail if it was optional and empty, else back
    /// to `head`.
    LoopTail { lp: u32, head: u32 },
    /// A quantified single character class.
    Star {
        set: u32,
        min: u32,
        max: u32,
        greedy: bool,
        back: bool,
        /// No backed-off position can begin the continuation.
        possessive: bool,
        /// Characters the continuation can begin with (`NO_SET` when
        /// unknown): back-off skips positions that start with another.
        follow: u32,
    },
    /// A lookaround whose body follows, up to its `LookEnd`; the
    /// continuation is at `end`.
    Look { negate: bool, end: u32 },
    LookEnd,
    BackRef { refs: u32, icase: bool, back: bool },
    Match,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Loop {
    pub min: u32,
    pub max: u32,
    pub greedy: bool,
    /// Captures inside the body: groups `first..first + count`.
    pub first: u32,
    pub count: u32,
}

/// A compiled backtracking program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Prog {
    pub insts: Vec<Inst>,
    pub sets: Vec<SetMatcher>,
    pub lits: Vec<u16>,
    pub loops: Vec<Loop>,
    pub refs: Vec<Vec<u32>>,
    pub groups: u32,
    pub unicode: bool,
    /// Characters every match begins with, when a match cannot be empty.
    pub first: Option<u32>,
    /// The program begins with a possessive unbounded class loop, so a
    /// failed attempt rules out every start inside the run it scanned.
    pub skip_runs: bool,
}

// ---- compiling ---------------------------------------------------------------------------------

struct Compiler {
    insts: Vec<Inst>,
    sets: Vec<CharSet>,
    set_index: HashMap<CharSet, u32>,
    lits: Vec<u16>,
    loops: Vec<Loop>,
    refs: Vec<Vec<u32>>,
    unicode: bool,
}

/// A character that can join a literal string: in unicode mode a lone
/// surrogate cannot, since its code unit may be half of a pair in the
/// input.
fn literal_char(n: &Node, unicode: bool) -> Option<u32> {
    match n {
        Node::Set(s) => s.single_member().filter(|&c| !(unicode && (0xD800..0xE000).contains(&c))),
        _ => None,
    }
}

fn push_units(out: &mut Vec<u16>, c: u32) {
    if c > 0xFFFF {
        let v = c - 0x10000;
        out.push(0xD800 + (v >> 10) as u16);
        out.push(0xDC00 + (v & 0x3FF) as u16);
    } else {
        out.push(c as u16);
    }
}

impl Compiler {
    fn emit(&mut self, i: Inst) -> u32 {
        self.insts.push(i);
        self.insts.len() as u32 - 1
    }

    fn pc(&self) -> u32 {
        self.insts.len() as u32
    }

    fn set(&mut self, s: &CharSet) -> u32 {
        if let Some(&i) = self.set_index.get(s) {
            return i;
        }
        let i = self.sets.len() as u32;
        self.sets.push(s.clone());
        self.set_index.insert(s.clone(), i);
        i
    }

    fn char_node(&mut self, s: &CharSet, back: bool) {
        match s.single_member() {
            Some(c) => self.emit(Inst::Char { c, back }),
            None => {
                let set = self.set(s);
                self.emit(Inst::Set { set, back })
            }
        };
    }

    fn node(&mut self, n: &Node, back: bool) {
        match n {
            Node::Empty => {}
            Node::Set(s) => self.char_node(s, back),
            Node::Assert(a) => {
                let i = match *a {
                    Assertion::Start { multiline } => Inst::Start { multiline },
                    Assertion::End { multiline } => Inst::End { multiline },
                    Assertion::Word { negate, extended } => {
                        let set = self.set(&crate::parse::word_chars(extended));
                        Inst::Word { negate, set }
                    }
                };
                self.emit(i);
            }
            Node::Capture(g, body) => {
                self.emit(Inst::Open(*g as u32));
                self.node(body, back);
                self.emit(Inst::Close(*g as u32));
            }
            Node::Concat(items) => {
                // Runs of single characters become one string, matched
                // whole in either direction.
                let mut runs: Vec<Result<Vec<u32>, &Node>> = Vec::new();
                for x in items {
                    match (literal_char(x, self.unicode), runs.last_mut()) {
                        (Some(c), Some(Ok(run))) => run.push(c),
                        (Some(c), _) => runs.push(Ok(vec![c])),
                        (None, _) => runs.push(Err(x)),
                    }
                }
                if back {
                    runs.reverse();
                }
                for r in runs {
                    match r {
                        Ok(run) if run.len() == 1 => {
                            self.emit(Inst::Char { c: run[0], back });
                        }
                        Ok(run) => {
                            let start = self.lits.len() as u32;
                            for c in run {
                                push_units(&mut self.lits, c);
                            }
                            let len = self.lits.len() as u32 - start;
                            self.emit(Inst::Str { start, len, back });
                        }
                        Err(x) => self.node(x, back),
                    }
                }
            }
            Node::Alt(alts) => {
                let mut jumps = Vec::new();
                for (k, x) in alts.iter().enumerate() {
                    if k + 1 < alts.len() {
                        let split = self.emit(Inst::Split { alt: 0 });
                        self.node(x, back);
                        jumps.push(self.emit(Inst::Jmp(0)));
                        let next = self.pc();
                        self.insts[split as usize] = Inst::Split { alt: next };
                    } else {
                        self.node(x, back);
                    }
                }
                let end = self.pc();
                for j in jumps {
                    self.insts[j as usize] = Inst::Jmp(end);
                }
            }
            Node::Repeat { body, min, max, greedy, first, count } => {
                let max = max.unwrap_or(INFINITE);
                if max == 0 {
                    return;
                }
                if let Node::Set(s) = &**body {
                    let set = self.set(s);
                    self.emit(Inst::Star {
                        set,
                        min: *min,
                        max,
                        greedy: *greedy,
                        back,
                        possessive: false,
                        follow: NO_SET,
                    });
                    return;
                }
                if *min == 1 && max == 1 {
                    // Its captures are unset already: nothing before this
                    // point in the same iteration of any enclosing loop
                    // can have set them.
                    self.node(body, back);
                    return;
                }
                let lp = self.loops.len() as u32;
                self.loops.push(Loop {
                    min: *min,
                    max,
                    greedy: *greedy,
                    first: *first as u32,
                    count: *count as u32,
                });
                self.emit(Inst::LoopInit(lp));
                let head = self.emit(Inst::LoopHead { lp, exit: 0 });
                self.emit(Inst::LoopBegin(lp));
                self.node(body, back);
                self.emit(Inst::LoopTail { lp, head });
                let exit = self.pc();
                self.insts[head as usize] = Inst::LoopHead { lp, exit };
            }
            Node::BackRef { groups, icase } => {
                let refs = self.refs.len() as u32;
                self.refs.push(groups.iter().map(|&g| g as u32).collect());
                self.emit(Inst::BackRef { refs, icase: *icase, back });
            }
            Node::Look { behind, negate, body, .. } => {
                let look = self.emit(Inst::Look { negate: *negate, end: 0 });
                self.node(body, *behind);
                self.emit(Inst::LookEnd);
                let end = self.pc();
                self.insts[look as usize] = Inst::Look { negate: *negate, end };
            }
        }
    }

    /// The characters a match from `pc` can consume first, or `None` when
    /// it may consume nothing (or the analysis cannot tell).
    fn first_from(&self, pc: u32) -> Option<CharSet> {
        let mut seen = vec![false; self.insts.len()];
        let mut stack = vec![pc];
        let mut out = CharSet::new();
        while let Some(pc) = stack.pop() {
            let pc = pc as usize;
            if std::mem::replace(&mut seen[pc], true) {
                continue;
            }
            match self.insts[pc] {
                Inst::Char { c, .. } => out = out.union(&CharSet::single(c)),
                Inst::Set { set, .. } => out = out.union(&self.sets[set as usize]),
                Inst::Str { start, len, back } => {
                    let units = &self.lits[start as usize..(start + len) as usize];
                    let c = if back { last_char(units, self.unicode) } else { first_char(units, self.unicode) };
                    out = out.union(&CharSet::single(c));
                }
                Inst::Star { set, min, .. } => {
                    out = out.union(&self.sets[set as usize]);
                    if min == 0 {
                        stack.push(pc as u32 + 1);
                    }
                }
                Inst::Split { alt } => {
                    stack.push(alt);
                    stack.push(pc as u32 + 1);
                }
                Inst::Jmp(t) => stack.push(t),
                Inst::LoopHead { exit, .. } => {
                    stack.push(exit);
                    stack.push(pc as u32 + 1);
                }
                Inst::LoopTail { head, .. } => stack.push(head),
                Inst::Open(_)
                | Inst::Close(_)
                | Inst::Start { .. }
                | Inst::End { .. }
                | Inst::Word { .. }
                | Inst::LoopInit(_)
                | Inst::LoopBegin(_) => stack.push(pc as u32 + 1),
                Inst::Look { .. } | Inst::LookEnd | Inst::BackRef { .. } | Inst::Match => return None,
            }
        }
        Some(out)
    }
}

fn first_char(units: &[u16], unicode: bool) -> u32 {
    match units {
        [h, l, ..] if unicode && (0xD800..0xDC00).contains(h) && (0xDC00..0xE000).contains(l) => {
            0x10000 + (((*h as u32) - 0xD800) << 10) + ((*l as u32) - 0xDC00)
        }
        [u, ..] => *u as u32,
        [] => 0,
    }
}

fn last_char(units: &[u16], unicode: bool) -> u32 {
    match units {
        [.., h, l] if unicode && (0xD800..0xDC00).contains(h) && (0xDC00..0xE000).contains(l) => {
            0x10000 + (((*h as u32) - 0xD800) << 10) + ((*l as u32) - 0xDC00)
        }
        [.., u] => *u as u32,
        [] => 0,
    }
}

impl Prog {
    pub(crate) fn compile(p: &Pattern) -> Prog {
        let mut c = Compiler {
            insts: Vec::new(),
            sets: Vec::new(),
            set_index: HashMap::new(),
            lits: Vec::new(),
            loops: Vec::new(),
            refs: Vec::new(),
            unicode: p.unicode,
        };
        c.node(&p.node, false);
        c.emit(Inst::Match);
        // Loops over a single class: possessive when no backed-off
        // position can start the continuation, else a filter for back-off.
        for pc in 0..c.insts.len() {
            if let Inst::Star { set, min, max, greedy, back, .. } = c.insts[pc] {
                let follow = c.first_from(pc as u32 + 1);
                let (possessive, follow) = match follow {
                    Some(f) if !f.overlaps(&c.sets[set as usize]) => (greedy, c.set(&f)),
                    Some(f) => (false, c.set(&f)),
                    None => (false, NO_SET),
                };
                c.insts[pc] = Inst::Star { set, min, max, greedy, back, possessive, follow };
            }
        }
        let first = c.first_from(0).map(|f| c.set(&f));
        let skip_runs = matches!(
            c.insts[0],
            Inst::Star { min: 0 | 1, max: INFINITE, greedy: true, back: false, possessive: true, .. }
        );
        Prog {
            skip_runs,
            insts: c.insts,
            sets: c.sets.iter().map(SetMatcher::new).collect(),
            lits: c.lits,
            loops: c.loops,
            refs: c.refs,
            groups: p.group_count() as u32,
            unicode: p.unicode,
            first,
        }
    }

    fn nregs(&self) -> usize {
        // Capture slots (two per group, group 0 included), the open
        // positions (one per group), two per loop (count, iteration
        // start).
        let g = self.groups as usize + 1;
        3 * g + 2 * self.loops.len()
    }
}

// ---- matching ----------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Frame {
    Choice { pc: u32, pos: usize },
    Restore { reg: u32, val: usize },
    /// A greedy class loop at `pc` that can give back characters down to
    /// `min_pos`; it now stands at `pos`.
    Greedy { pc: u32, pos: usize, min_pos: usize },
    /// A lazy class loop at `pc` that has taken `count` characters to
    /// `pos`.
    Lazy { pc: u32, pos: usize, count: u32 },
    /// The lookaround at `pc` was entered at `pos`.
    Look { pc: u32, pos: usize },
}

/// Per-regex working memory, reused across matches.
#[derive(Default)]
pub(crate) struct Scratch {
    regs: Vec<usize>,
    stack: Vec<Frame>,
    looks: Vec<usize>,
    /// Where the class loop the program begins with stopped scanning.
    run_end: usize,
}

/// How a search ended.
pub(crate) enum Outcome {
    Match(Vec<Option<(usize, usize)>>),
    NoMatch,
    /// The step budget ran out while trying the start position given;
    /// every earlier start position has failed.
    Exhausted(usize),
}

#[inline]
fn is_lt(u: u32) -> bool {
    matches!(u, 0x0A | 0x0D | 0x2028 | 0x2029)
}

/// The character at `pos` and its width in code units.
#[inline]
pub(crate) fn decode(input: &[u16], pos: usize, unicode: bool) -> Option<(u32, usize)> {
    let u = *input.get(pos)? as u32;
    if unicode && (0xD800..0xDC00).contains(&u) {
        if let Some(&l) = input.get(pos + 1) {
            let l = l as u32;
            if (0xDC00..0xE000).contains(&l) {
                return Some((0x10000 + ((u - 0xD800) << 10) + (l - 0xDC00), 2));
            }
        }
    }
    Some((u, 1))
}

/// The character that ends at `pos` and its width.
#[inline]
pub(crate) fn decode_left(input: &[u16], pos: usize, unicode: bool) -> Option<(u32, usize)> {
    if pos == 0 || pos > input.len() {
        return None;
    }
    let u = input[pos - 1] as u32;
    if unicode && (0xDC00..0xE000).contains(&u) && pos >= 2 {
        let h = input[pos - 2] as u32;
        if (0xD800..0xDC00).contains(&h) {
            return Some((0x10000 + ((h - 0xD800) << 10) + (u - 0xDC00), 2));
        }
    }
    Some((u, 1))
}

impl Prog {
    #[inline]
    fn read(&self, input: &[u16], pos: usize, back: bool) -> Option<(u32, usize)> {
        if back {
            decode_left(input, pos, self.unicode)
        } else {
            decode(input, pos, self.unicode)
        }
    }

    #[inline]
    fn step(pos: usize, w: usize, back: bool) -> usize {
        if back {
            pos - w
        } else {
            pos + w
        }
    }

    /// Search from `start` (only at `start` when `sticky`), spending at
    /// most `budget` steps.
    pub(crate) fn exec(&self, s: &mut Scratch, input: &[u16], start: usize, sticky: bool, budget: u64) -> Outcome {
        if start > input.len() {
            return Outcome::NoMatch;
        }
        let nregs = self.nregs();
        if s.regs.len() != nregs {
            s.regs = vec![NONE; nregs];
        }
        let mut steps = budget;
        let mut pos = start;
        loop {
            if !sticky {
                if let Some(f) = self.first {
                    let f = &self.sets[f as usize];
                    loop {
                        match decode(input, pos, self.unicode) {
                            Some((c, _)) if f.contains(c) => break,
                            Some((_, w)) => pos += w,
                            None => return Outcome::NoMatch,
                        }
                    }
                }
            }
            match self.run(s, input, pos, &mut steps) {
                Some(true) => {
                    let g = self.groups as usize + 1;
                    let caps = (0..g)
                        .map(|k| match (s.regs[2 * k], s.regs[2 * k + 1]) {
                            (a, b) if a != NONE && b != NONE => Some((a, b)),
                            _ => None,
                        })
                        .collect();
                    return Outcome::Match(caps);
                }
                Some(false) => {}
                None => return Outcome::Exhausted(pos),
            }
            if sticky {
                return Outcome::NoMatch;
            }
            if self.skip_runs && s.run_end > pos {
                // The attempt began with a possessive class loop that ran
                // to `run_end`. An attempt from inside that run would run
                // to the same place and fail the same way.
                pos = s.run_end;
                continue;
            }
            match decode(input, pos, self.unicode) {
                Some((_, w)) => pos += w,
                None => return Outcome::NoMatch,
            }
        }
    }

    /// One attempt at `start`: `Some(true)` with the captures in the
    /// registers, `Some(false)` on failure, `None` when out of steps.
    fn run(&self, s: &mut Scratch, input: &[u16], start: usize, steps: &mut u64) -> Option<bool> {
        let g = self.groups as usize + 1;
        s.regs[..3 * g].iter_mut().for_each(|r| *r = NONE);
        s.stack.clear();
        s.looks.clear();
        s.run_end = 0;
        s.regs[0] = start;
        let regs = &mut s.regs;
        let stack = &mut s.stack;
        let looks = &mut s.looks;
        let run_end = &mut s.run_end;
        let open_base = 2 * g;
        let loop_base = 3 * g;
        let mut pc: usize = 0;
        let mut pos = start;
        macro_rules! fail {
            () => {
                match self.backtrack(regs, stack, looks, input, steps) {
                    Some((p, q)) => {
                        pc = p;
                        pos = q;
                        continue;
                    }
                    None => return if *steps == 0 { None } else { Some(false) },
                }
            };
        }
        macro_rules! set_reg {
            ($r:expr, $v:expr) => {{
                let r = $r;
                stack.push(Frame::Restore { reg: r as u32, val: regs[r] });
                regs[r] = $v;
            }};
        }
        loop {
            if *steps == 0 {
                return None;
            }
            *steps -= 1;
            match self.insts[pc] {
                Inst::Char { c, back } => match self.read(input, pos, back) {
                    Some((x, w)) if x == c => {
                        pos = Self::step(pos, w, back);
                        pc += 1;
                    }
                    _ => fail!(),
                },
                Inst::Set { set, back } => match self.read(input, pos, back) {
                    Some((x, w)) if self.sets[set as usize].contains(x) => {
                        pos = Self::step(pos, w, back);
                        pc += 1;
                    }
                    _ => fail!(),
                },
                Inst::Str { start, len, back } => {
                    let lit = &self.lits[start as usize..(start + len) as usize];
                    let len = len as usize;
                    // Literals hold no surrogate code points, so equal
                    // units are equal characters even in unicode mode.
                    let ok = if back {
                        pos >= len && &input[pos - len..pos] == lit
                    } else {
                        pos + len <= input.len() && &input[pos..pos + len] == lit
                    };
                    if !ok {
                        fail!();
                    }
                    pos = if back { pos - len } else { pos + len };
                    pc += 1;
                }
                Inst::Split { alt } => {
                    stack.push(Frame::Choice { pc: alt, pos });
                    pc += 1;
                }
                Inst::Jmp(t) => pc = t as usize,
                Inst::Open(k) => {
                    set_reg!(open_base + k as usize, pos);
                    pc += 1;
                }
                Inst::Close(k) => {
                    let o = regs[open_base + k as usize];
                    let (a, b) = if o <= pos { (o, pos) } else { (pos, o) };
                    set_reg!(2 * k as usize, a);
                    set_reg!(2 * k as usize + 1, b);
                    pc += 1;
                }
                Inst::Start { multiline } => {
                    if pos == 0 || (multiline && is_lt(input[pos - 1] as u32)) {
                        pc += 1;
                    } else {
                        fail!();
                    }
                }
                Inst::End { multiline } => {
                    if pos == input.len() || (multiline && is_lt(input[pos] as u32)) {
                        pc += 1;
                    } else {
                        fail!();
                    }
                }
                Inst::Word { negate, set } => {
                    let w = &self.sets[set as usize];
                    let a = decode_left(input, pos, self.unicode).is_some_and(|(c, _)| w.contains(c));
                    let b = decode(input, pos, self.unicode).is_some_and(|(c, _)| w.contains(c));
                    if (a != b) != negate {
                        pc += 1;
                    } else {
                        fail!();
                    }
                }
                Inst::LoopInit(lp) => {
                    set_reg!(loop_base + 2 * lp as usize, 0);
                    pc += 1;
                }
                Inst::LoopHead { lp, exit } => {
                    let l = &self.loops[lp as usize];
                    let count = regs[loop_base + 2 * lp as usize] as u64;
                    if count >= l.max as u64 && l.max != INFINITE {
                        pc = exit as usize;
                    } else if count < l.min as u64 {
                        pc += 1;
                    } else if l.greedy {
                        stack.push(Frame::Choice { pc: exit, pos });
                        pc += 1;
                    } else {
                        stack.push(Frame::Choice { pc: pc as u32 + 1, pos });
                        pc = exit as usize;
                    }
                }
                Inst::LoopBegin(lp) => {
                    let l = self.loops[lp as usize];
                    let r = loop_base + 2 * lp as usize;
                    let count = regs[r];
                    set_reg!(r, count.saturating_add(1));
                    set_reg!(r + 1, pos);
                    for k in l.first..l.first + l.count {
                        let k = k as usize;
                        if regs[2 * k] != NONE {
                            set_reg!(2 * k, NONE);
                            set_reg!(2 * k + 1, NONE);
                        }
                    }
                    pc += 1;
                }
                Inst::LoopTail { lp, head } => {
                    let l = &self.loops[lp as usize];
                    let r = loop_base + 2 * lp as usize;
                    if regs[r] as u64 > l.min as u64 && regs[r + 1] == pos {
                        fail!();
                    }
                    pc = head as usize;
                }
                Inst::Star { set, min, max, greedy, back, possessive, follow } => {
                    let m = &self.sets[set as usize];
                    if greedy {
                        let mut n = 0u32;
                        let mut p = pos;
                        let mut min_pos = if min == 0 { pos } else { NONE };
                        while n < max {
                            match self.read(input, p, back) {
                                Some((x, w)) if m.contains(x) => {
                                    p = Self::step(p, w, back);
                                    n += 1;
                                    if n == min {
                                        min_pos = p;
                                    }
                                }
                                _ => break,
                            }
                        }
                        if n < min {
                            fail!();
                        }
                        *steps = steps.saturating_sub(n as u64);
                        if pc == 0 {
                            *run_end = p;
                        }
                        if !possessive && p != min_pos {
                            stack.push(Frame::Greedy { pc: pc as u32, pos: p, min_pos });
                        }
                        // A follow filter: if the continuation cannot start
                        // here, back off right away.
                        if follow != NO_SET && !possessive {
                            let f = &self.sets[follow as usize];
                            if !self.read(input, p, back).is_some_and(|(c, _)| f.contains(c)) {
                                fail!();
                            }
                        }
                        pos = p;
                        pc += 1;
                    } else {
                        let mut p = pos;
                        let mut n = 0;
                        while n < min {
                            match self.read(input, p, back) {
                                Some((x, w)) if m.contains(x) => p = Self::step(p, w, back),
                                _ => break,
                            }
                            n += 1;
                        }
                        if n < min {
                            // (`fail!` cannot run inside the loop above:
                            // its `continue` would bind to that loop.)
                            fail!();
                        }
                        if min < max {
                            stack.push(Frame::Lazy { pc: pc as u32, pos: p, count: min });
                        }
                        if follow != NO_SET {
                            let f = &self.sets[follow as usize];
                            if !self.read(input, p, back).is_some_and(|(c, _)| f.contains(c)) {
                                fail!();
                            }
                        }
                        pos = p;
                        pc += 1;
                    }
                }
                Inst::Look { .. } => {
                    stack.push(Frame::Look { pc: pc as u32, pos });
                    looks.push(stack.len() - 1);
                    pc += 1;
                }
                Inst::LookEnd => {
                    let idx = looks.pop().expect("lookaround marker");
                    let Frame::Look { pc: lpc, pos: lpos } = stack[idx] else { unreachable!() };
                    let Inst::Look { negate, end } = self.insts[lpc as usize] else { unreachable!() };
                    if negate {
                        // The body matched, so the lookaround fails: undo
                        // it and backtrack past it.
                        while stack.len() > idx + 1 {
                            if let Some(Frame::Restore { reg, val }) = stack.pop() {
                                regs[reg as usize] = val;
                            }
                        }
                        stack.pop();
                        fail!();
                    }
                    // Keep the body's register undo records, drop its
                    // choice points.
                    let mut w = idx;
                    for r in idx + 1..stack.len() {
                        if let Frame::Restore { .. } = stack[r] {
                            stack[w] = stack[r];
                            w += 1;
                        }
                    }
                    stack.truncate(w);
                    pos = lpos;
                    pc = end as usize;
                }
                Inst::BackRef { refs, icase, back } => {
                    let group = self.refs[refs as usize]
                        .iter()
                        .map(|&k| (regs[2 * k as usize], regs[2 * k as usize + 1]))
                        .find(|&(a, b)| a != NONE && b != NONE);
                    match group {
                        None => pc += 1,
                        Some((a, b)) => match self.backref(input, a, b, pos, icase, back) {
                            Some(p) => {
                                pos = p;
                                pc += 1;
                            }
                            None => fail!(),
                        },
                    }
                }
                Inst::Match => {
                    regs[1] = pos;
                    return Some(true);
                }
            }
        }
    }

    /// Match the captured `input[a..b]` again at `pos`: the position after
    /// it, or `None`.
    fn backref(&self, input: &[u16], a: usize, b: usize, pos: usize, icase: bool, back: bool) -> Option<usize> {
        let len = b - a;
        if !icase {
            return if back {
                (pos >= len && input[pos - len..pos] == input[a..b]).then(|| pos - len)
            } else {
                (pos + len <= input.len() && input[pos..pos + len] == input[a..b]).then_some(pos + len)
            };
        }
        let canon = |c: u32| unicode::canonicalize(c, self.unicode);
        if back {
            let (mut i, mut p) = (b, pos);
            while i > a {
                let (x, wx) = decode_left(&input[..i], i, self.unicode)?;
                let (y, wy) = decode_left(input, p, self.unicode)?;
                if canon(x) != canon(y) {
                    return None;
                }
                i -= wx;
                p -= wy;
            }
            Some(p)
        } else {
            let (mut i, mut p) = (a, pos);
            while i < b {
                let (x, wx) = decode(&input[..b], i, self.unicode)?;
                let (y, wy) = decode(input, p, self.unicode)?;
                if canon(x) != canon(y) {
                    return None;
                }
                i += wx;
                p += wy;
            }
            Some(p)
        }
    }

    /// Pop the stack to the next choice point: its pc and position, or
    /// `None` when the attempt has failed (or the steps ran out).
    fn backtrack(
        &self,
        regs: &mut [usize],
        stack: &mut Vec<Frame>,
        looks: &mut Vec<usize>,
        input: &[u16],
        steps: &mut u64,
    ) -> Option<(usize, usize)> {
        while let Some(f) = stack.pop() {
            if *steps == 0 {
                return None;
            }
            *steps -= 1;
            match f {
                Frame::Restore { reg, val } => regs[reg as usize] = val,
                Frame::Choice { pc, pos } => return Some((pc as usize, pos)),
                Frame::Greedy { pc, mut pos, min_pos } => {
                    let Inst::Star { follow, back, .. } = self.insts[pc as usize] else { unreachable!() };
                    let f = (follow != NO_SET).then(|| &self.sets[follow as usize]);
                    // Give back one character at a time, skipping the
                    // positions the continuation cannot start at.
                    loop {
                        if pos == min_pos {
                            break;
                        }
                        let (_, w) = if back {
                            decode(input, pos, self.unicode)?
                        } else {
                            decode_left(input, pos, self.unicode)?
                        };
                        pos = if back { pos + w } else { pos - w };
                        if f.is_none_or(|f| self.read(input, pos, back).is_some_and(|(c, _)| f.contains(c))) {
                            if pos != min_pos {
                                stack.push(Frame::Greedy { pc, pos, min_pos });
                            }
                            return Some((pc as usize + 1, pos));
                        }
                    }
                }
                Frame::Lazy { pc, mut pos, mut count } => {
                    let Inst::Star { set, max, back, follow, .. } = self.insts[pc as usize] else { unreachable!() };
                    let m = &self.sets[set as usize];
                    let f = (follow != NO_SET).then(|| &self.sets[follow as usize]);
                    loop {
                        if count >= max {
                            break;
                        }
                        match self.read(input, pos, back) {
                            Some((x, w)) if m.contains(x) => {
                                pos = Self::step(pos, w, back);
                                count += 1;
                            }
                            _ => break,
                        }
                        if f.is_none_or(|f| self.read(input, pos, back).is_some_and(|(c, _)| f.contains(c))) {
                            if count < max {
                                stack.push(Frame::Lazy { pc, pos, count });
                            }
                            return Some((pc as usize + 1, pos));
                        }
                    }
                }
                Frame::Look { pc, pos } => {
                    looks.pop();
                    let Inst::Look { negate, end } = self.insts[pc as usize] else { unreachable!() };
                    if negate {
                        // The body failed: the negative lookaround holds.
                        return Some((end as usize, pos));
                    }
                }
            }
        }
        None
    }
}
