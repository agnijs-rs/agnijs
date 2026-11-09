//! Compiled programs as bytes (ADR 0010 addendum, TASK-100.31).
//!
//! Bytecode embeds the program of each regex literal so loading it skips
//! parsing and compiling the pattern. The program does not depend on the
//! matcher mode. Bytes from anywhere are verified before use: decoding
//! checks every index, jump target and structural rule the matchers rely
//! on (see [`verify_backtrack`] and [`verify_linear`]), so a corrupt
//! program is an error, never a panic or a match that cannot finish. (A
//! verified program can still be as slow as some compiled patterns are
//! under backtracking, which is what the step budget of `Auto` is for.)
//!
//! Layout (integers are unsigned LEB128, flags one byte, 0 or 1):
//!
//! ```text
//! version (1)
//! names:     count, then per group 0 (unnamed) or 1, length, UTF-8
//! starts_anchored
//! backtrack: unicode, groups, first (0 = none, else set + 1), skip_runs,
//!            sets (count, then per set: ranges, then lo and hi - lo each),
//!            lits (count, units), loops (count, min, max, greedy, first,
//!            count), refs (count, then per ref: groups, group numbers),
//!            insts (count, then a tag and the operands each)
//! linear:    0 and an ineligibility reason, or 1, a length, and the
//!            program (so a build without the linear matcher can skip it)
//! ```

use crate::backtrack::{self, Inst, Loop};
use crate::charset::{CharSet, SetMatcher, MAX_CODE_POINT};
use crate::Program;

const VERSION: u8 = 1;

/// Why a pattern is not eligible for the linear matcher, by code. Every
/// reason `linear` can give must be here (a unit test checks).
#[cfg_attr(not(feature = "linear"), allow(dead_code))]
const REASONS: &[&str] = &[
    "the linear matcher is compiled out",
    "program too large",
    "backreference",
    "lookaround",
    "quantifier bounds too large to unroll",
    "nullable quantifiers nested too deeply",
    "thread table too large",
];

// ---- writing ---------------------------------------------------------------------------------

#[derive(Default)]
struct Writer(Vec<u8>);

impl Writer {
    fn uint(&mut self, mut v: u64) {
        loop {
            let b = (v & 0x7F) as u8;
            v >>= 7;
            if v == 0 {
                self.0.push(b);
                return;
            }
            self.0.push(b | 0x80);
        }
    }
    fn flag(&mut self, b: bool) {
        self.0.push(b as u8);
    }
    fn sets(&mut self, sets: &[SetMatcher]) {
        self.uint(sets.len() as u64);
        for s in sets {
            let set = s.to_set();
            self.uint(set.ranges().len() as u64);
            for &(lo, hi) in set.ranges() {
                self.uint(lo as u64);
                self.uint((hi - lo) as u64);
            }
        }
    }
}

pub(crate) fn encode(p: &Program) -> Vec<u8> {
    let mut w = Writer::default();
    w.0.push(VERSION);
    w.uint(p.names.len() as u64);
    for n in &p.names {
        match n {
            None => w.0.push(0),
            Some(s) => {
                w.0.push(1);
                w.uint(s.len() as u64);
                w.0.extend_from_slice(s.as_bytes());
            }
        }
    }
    w.flag(p.starts_anchored);
    backtrack_part(&mut w, &p.backtrack);
    #[cfg(feature = "linear")]
    match &p.linear {
        Ok(lin) => {
            let mut body = Writer::default();
            linear_part(&mut body, lin);
            w.0.push(1);
            w.uint(body.0.len() as u64);
            w.0.extend_from_slice(&body.0);
        }
        Err(reason) => {
            w.0.push(0);
            w.uint(reason_code(reason) as u64);
        }
    }
    #[cfg(not(feature = "linear"))]
    {
        w.0.push(0);
        w.uint(0);
    }
    w.0
}

#[cfg(feature = "linear")]
fn reason_code(reason: &str) -> usize {
    let code = REASONS.iter().position(|r| *r == reason);
    debug_assert!(code.is_some(), "unknown ineligibility reason {reason:?}");
    code.unwrap_or(0)
}

fn backtrack_part(w: &mut Writer, b: &backtrack::Prog) {
    w.flag(b.unicode);
    w.uint(b.groups as u64);
    w.uint(b.first.map_or(0, |f| f as u64 + 1));
    w.flag(b.skip_runs);
    w.sets(&b.sets);
    w.uint(b.lits.len() as u64);
    for &u in &b.lits {
        w.uint(u as u64);
    }
    w.uint(b.loops.len() as u64);
    for l in &b.loops {
        w.uint(l.min as u64);
        w.uint(l.max as u64);
        w.flag(l.greedy);
        w.uint(l.first as u64);
        w.uint(l.count as u64);
    }
    w.uint(b.refs.len() as u64);
    for r in &b.refs {
        w.uint(r.len() as u64);
        for &g in r {
            w.uint(g as u64);
        }
    }
    w.uint(b.insts.len() as u64);
    for &i in &b.insts {
        let (tag, ops): (u8, &[u64]) = match i {
            Inst::Char { c, back } => (0, &[c as u64, back as u64]),
            Inst::Set { set, back } => (1, &[set as u64, back as u64]),
            Inst::Str { start, len, back } => (2, &[start as u64, len as u64, back as u64]),
            Inst::Split { alt } => (3, &[alt as u64]),
            Inst::Jmp(t) => (4, &[t as u64]),
            Inst::Open(g) => (5, &[g as u64]),
            Inst::Close(g) => (6, &[g as u64]),
            Inst::Start { multiline } => (7, &[multiline as u64]),
            Inst::End { multiline } => (8, &[multiline as u64]),
            Inst::Word { negate, set } => (9, &[negate as u64, set as u64]),
            Inst::LoopInit(lp) => (10, &[lp as u64]),
            Inst::LoopHead { lp, exit } => (11, &[lp as u64, exit as u64]),
            Inst::LoopBegin(lp) => (12, &[lp as u64]),
            Inst::LoopTail { lp, head } => (13, &[lp as u64, head as u64]),
            Inst::Star { set, min, max, greedy, back, possessive, follow } => (
                14,
                &[set as u64, min as u64, max as u64, greedy as u64, back as u64, possessive as u64, follow as u64],
            ),
            Inst::Look { negate, end } => (15, &[negate as u64, end as u64]),
            Inst::LookEnd => (16, &[]),
            Inst::BackRef { refs, icase, back } => (17, &[refs as u64, icase as u64, back as u64]),
            Inst::Match => (18, &[]),
        };
        w.0.push(tag);
        for &o in ops {
            w.uint(o);
        }
    }
}

#[cfg(feature = "linear")]
fn linear_part(w: &mut Writer, l: &crate::linear::Linear) {
    use crate::linear::Inst;
    w.uint(l.prog.len() as u64);
    for &i in &l.prog {
        let (tag, ops): (u8, &[u64]) = match i {
            Inst::Lit(c) => (0, &[c as u64]),
            Inst::Set(s) => (1, &[s as u64]),
            Inst::Split(a, b) => (2, &[a as u64, b as u64]),
            Inst::Jmp(t) => (3, &[t as u64]),
            Inst::Save(k) => (4, &[k as u64]),
            Inst::Reset(lo, hi) => (5, &[lo as u64, hi as u64]),
            Inst::Start { multiline } => (6, &[multiline as u64]),
            Inst::End { multiline } => (7, &[multiline as u64]),
            Inst::Word { invert, set } => (8, &[invert as u64, set as u64]),
            Inst::Enter(d) => (9, &[d as u64]),
            Inst::Check(d) => (10, &[d as u64]),
            Inst::Match => (11, &[]),
        };
        w.0.push(tag);
        for &o in ops {
            w.uint(o);
        }
    }
    w.sets(&l.sets);
    w.uint(l.shift as u64);
    w.uint(l.nslots as u64);
    w.flag(l.unicode);
    w.flag(l.nested);
    match &l.first {
        None => w.0.push(0),
        Some(first) => {
            w.0.push(1);
            w.uint(first.len() as u64);
            for &i in first {
                match i {
                    Inst::Lit(c) => {
                        w.0.push(0);
                        w.uint(c as u64);
                    }
                    Inst::Set(s) => {
                        w.0.push(1);
                        w.uint(s as u64);
                    }
                    // `first` holds consuming instructions only; the
                    // reader rejects anything else.
                    _ => w.0.push(2),
                }
            }
        }
    }
    w.uint(l.relevant.len() as u64);
    for &r in &l.relevant {
        w.uint(r as u64);
    }
}

// ---- reading ---------------------------------------------------------------------------------

type Result<T> = std::result::Result<T, &'static str>;

#[cfg(feature = "linear")]
type Lin = crate::linear::Linear;
#[cfg(not(feature = "linear"))]
type Lin = ();

const TRUNCATED: &str = "truncated";

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> Result<u8> {
        let b = *self.bytes.get(self.at).ok_or(TRUNCATED)?;
        self.at += 1;
        Ok(b)
    }
    fn uint(&mut self) -> Result<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            v |= ((b & 0x7F) as u64) << shift;
            if b & 0x80 == 0 {
                // The tenth byte may only carry the top bit.
                if shift == 63 && b > 1 {
                    break;
                }
                return Ok(v);
            }
        }
        Err("integer too long")
    }
    fn u32(&mut self) -> Result<u32> {
        u32::try_from(self.uint()?).map_err(|_| "integer out of range")
    }
    fn flag(&mut self) -> Result<bool> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err("bad flag"),
        }
    }
    /// A count of items that each take at least `min_bytes` bytes, so a
    /// huge count in a short input fails before anything is allocated.
    fn count(&mut self, min_bytes: usize) -> Result<usize> {
        let n = self.uint()?;
        let left = (self.bytes.len() - self.at) as u64;
        if n.saturating_mul(min_bytes as u64) > left {
            return Err(TRUNCATED);
        }
        Ok(n as usize)
    }
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.bytes.len()).ok_or(TRUNCATED)?;
        let s = &self.bytes[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn sets(&mut self) -> Result<Vec<SetMatcher>> {
        let n = self.count(1)?;
        let mut sets = Vec::with_capacity(n);
        for _ in 0..n {
            let k = self.count(2)?;
            let mut ranges = Vec::with_capacity(k);
            let mut prev: Option<u32> = None;
            for _ in 0..k {
                let lo = self.u32()?;
                let hi = lo.checked_add(self.u32()?).ok_or("bad range")?;
                // Sorted and apart, as the writer leaves them, so the set
                // reads back as the same matcher.
                if hi > MAX_CODE_POINT || prev.is_some_and(|p| lo <= p.saturating_add(1)) {
                    return Err("bad range");
                }
                prev = Some(hi);
                ranges.push((lo, hi));
            }
            sets.push(SetMatcher::new(&CharSet::from_unsorted(ranges)));
        }
        Ok(sets)
    }
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Program> {
    let mut r = Reader { bytes, at: 0 };
    if r.byte()? != VERSION {
        return Err("unknown program version");
    }
    let n = r.count(1)?;
    let mut names = Vec::with_capacity(n);
    for _ in 0..n {
        names.push(match r.byte()? {
            0 => None,
            1 => {
                let len = r.count(1)?;
                let s = std::str::from_utf8(r.take(len)?).map_err(|_| "bad group name")?;
                Some(s.to_string())
            }
            _ => return Err("bad group name"),
        });
    }
    let starts_anchored = r.flag()?;
    let backtrack = read_backtrack(&mut r)?;
    if backtrack.groups as usize != names.len() {
        return Err("group count mismatch");
    }
    verify_backtrack(&backtrack)?;
    let linear: Result<Lin> = match r.byte()? {
        0 => {
            let code = r.uint()?;
            Err(*REASONS.get(code as usize).ok_or("unknown ineligibility reason")?)
        }
        1 => {
            let len = r.count(1)?;
            let body = r.take(len)?;
            #[cfg(feature = "linear")]
            {
                let mut b = Reader { bytes: body, at: 0 };
                let lin = read_linear(&mut b)?;
                if b.at != body.len() {
                    return Err("trailing bytes");
                }
                verify_linear(&lin, names.len())?;
                Ok(lin)
            }
            #[cfg(not(feature = "linear"))]
            {
                let _ = body;
                Err(REASONS[0])
            }
        }
        _ => return Err("bad linear program tag"),
    };
    if r.at != bytes.len() {
        return Err("trailing bytes");
    }
    #[cfg(not(feature = "linear"))]
    let _ = linear;
    Ok(Program {
        names,
        starts_anchored,
        backtrack,
        #[cfg(feature = "linear")]
        linear,
    })
}

fn read_backtrack(r: &mut Reader) -> Result<backtrack::Prog> {
    let unicode = r.flag()?;
    let groups = r.u32()?;
    let first = match r.u32()? {
        0 => None,
        f => Some(f - 1),
    };
    let skip_runs = r.flag()?;
    let sets = r.sets()?;
    let n = r.count(1)?;
    let mut lits = Vec::with_capacity(n);
    for _ in 0..n {
        lits.push(u16::try_from(r.uint()?).map_err(|_| "bad code unit")?);
    }
    let n = r.count(5)?;
    let mut loops = Vec::with_capacity(n);
    for _ in 0..n {
        loops.push(Loop { min: r.u32()?, max: r.u32()?, greedy: r.flag()?, first: r.u32()?, count: r.u32()? });
    }
    let n = r.count(1)?;
    let mut refs = Vec::with_capacity(n);
    for _ in 0..n {
        let k = r.count(1)?;
        let mut v = Vec::with_capacity(k);
        for _ in 0..k {
            v.push(r.u32()?);
        }
        refs.push(v);
    }
    let n = r.count(1)?;
    let mut insts = Vec::with_capacity(n);
    for _ in 0..n {
        insts.push(match r.byte()? {
            0 => Inst::Char { c: r.u32()?, back: r.flag()? },
            1 => Inst::Set { set: r.u32()?, back: r.flag()? },
            2 => Inst::Str { start: r.u32()?, len: r.u32()?, back: r.flag()? },
            3 => Inst::Split { alt: r.u32()? },
            4 => Inst::Jmp(r.u32()?),
            5 => Inst::Open(r.u32()?),
            6 => Inst::Close(r.u32()?),
            7 => Inst::Start { multiline: r.flag()? },
            8 => Inst::End { multiline: r.flag()? },
            9 => Inst::Word { negate: r.flag()?, set: r.u32()? },
            10 => Inst::LoopInit(r.u32()?),
            11 => Inst::LoopHead { lp: r.u32()?, exit: r.u32()? },
            12 => Inst::LoopBegin(r.u32()?),
            13 => Inst::LoopTail { lp: r.u32()?, head: r.u32()? },
            14 => Inst::Star {
                set: r.u32()?,
                min: r.u32()?,
                max: r.u32()?,
                greedy: r.flag()?,
                back: r.flag()?,
                possessive: r.flag()?,
                follow: r.u32()?,
            },
            15 => Inst::Look { negate: r.flag()?, end: r.u32()? },
            16 => Inst::LookEnd,
            17 => Inst::BackRef { refs: r.u32()?, icase: r.flag()?, back: r.flag()? },
            18 => Inst::Match,
            _ => return Err("bad instruction"),
        });
    }
    Ok(backtrack::Prog { insts, sets, lits, loops, refs, groups, unicode, first, skip_runs })
}

#[cfg(feature = "linear")]
fn read_linear(r: &mut Reader) -> Result<crate::linear::Linear> {
    use crate::linear::Inst;
    let n = r.count(1)?;
    let mut prog = Vec::with_capacity(n);
    let depth = |d: u64| u8::try_from(d).map_err(|_| "bad nesting level");
    for _ in 0..n {
        prog.push(match r.byte()? {
            0 => Inst::Lit(r.u32()?),
            1 => Inst::Set(r.u32()?),
            2 => Inst::Split(r.u32()?, r.u32()?),
            3 => Inst::Jmp(r.u32()?),
            4 => Inst::Save(r.u32()?),
            5 => Inst::Reset(r.u32()?, r.u32()?),
            6 => Inst::Start { multiline: r.flag()? },
            7 => Inst::End { multiline: r.flag()? },
            8 => Inst::Word { invert: r.flag()?, set: r.u32()? },
            9 => Inst::Enter(depth(r.uint()?)?),
            10 => Inst::Check(depth(r.uint()?)?),
            11 => Inst::Match,
            _ => return Err("bad instruction"),
        });
    }
    let sets = r.sets()?;
    let shift = r.u32()?;
    let nslots = usize::try_from(r.uint()?).map_err(|_| "bad slot count")?;
    let unicode = r.flag()?;
    let nested = r.flag()?;
    let first = match r.byte()? {
        0 => None,
        1 => {
            let k = r.count(2)?;
            let mut v = Vec::with_capacity(k);
            for _ in 0..k {
                v.push(match r.byte()? {
                    0 => Inst::Lit(r.u32()?),
                    1 => Inst::Set(r.u32()?),
                    _ => return Err("bad first instruction"),
                });
            }
            Some(v)
        }
        _ => return Err("bad first instructions"),
    };
    let k = r.count(1)?;
    let mut relevant = Vec::with_capacity(k);
    for _ in 0..k {
        relevant.push(r.u32()?);
    }
    Ok(crate::linear::Linear { prog, sets, shift, nslots, unicode, nested, first, relevant })
}

// ---- verifying -------------------------------------------------------------------------------

const BAD: &str = "malformed program";

fn check(ok: bool) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(BAD)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Top,
    Loop(u32),
    Look,
}

/// A loop (`LoopInit` to its `LoopTail`) or a lookaround (`Look` to its
/// `LookEnd`): the compiler emits them properly nested.
struct Region {
    kind: Kind,
    parent: usize,
    start: usize,
    /// Its last instruction.
    end: usize,
    /// The innermost lookaround (or the top level) it sits in, whose
    /// direction its characters are read in.
    look: usize,
}

/// What the backtracking matcher relies on beyond in-range indices: the
/// shape its compiler gives every program. Loops and lookarounds nest
/// properly and are entered only at their first instruction; every jump
/// goes forward except a `LoopTail` back to its own `LoopHead`, so every
/// cycle runs a loop iteration, which must consume input once past the
/// minimum; and the code of one lookaround (or the top level) reads in
/// one direction, so an iteration cannot step forward and back again.
/// Together these mean a verified program behaves like a compiled one:
/// it cannot index out of bounds and every attempt finishes.
pub(crate) fn verify_backtrack(b: &backtrack::Prog) -> Result<()> {
    let n = b.insts.len();
    let set = |s: u32| (s as usize) < b.sets.len();
    let group = |g: u32| g >= 1 && g <= b.groups;
    check(n > 0 && b.first.is_none_or(set))?;
    for l in &b.loops {
        check(l.count == 0 || (l.first >= 1 && l.first as u64 + l.count as u64 <= b.groups as u64 + 1))?;
    }
    for r in &b.refs {
        check(r.iter().all(|&g| group(g)))?;
    }
    let mut regions = vec![Region { kind: Kind::Top, parent: 0, start: 0, end: usize::MAX, look: 0 }];
    let mut region_of = vec![0usize; n];
    let mut open = vec![0usize];
    let mut loop_seen = vec![false; b.loops.len()];
    let mut dir: Vec<Option<bool>> = vec![Some(false)];
    for (pc, &inst) in b.insts.iter().enumerate() {
        let top = *open.last().ok_or(BAD)?;
        let mut here = top;
        let mut reads = None;
        match inst {
            Inst::Char { back, .. } => reads = Some(back),
            Inst::Set { set: s, back } => {
                check(set(s))?;
                reads = Some(back);
            }
            Inst::Str { start, len, back } => {
                check(start as u64 + len as u64 <= b.lits.len() as u64)?;
                reads = Some(back);
            }
            Inst::Star { set: s, follow, back, .. } => {
                check(set(s) && (follow == u32::MAX || set(follow)))?;
                reads = Some(back);
            }
            Inst::BackRef { refs, back, .. } => {
                check((refs as usize) < b.refs.len())?;
                reads = Some(back);
            }
            Inst::Word { set: s, .. } => check(set(s))?,
            Inst::Open(g) | Inst::Close(g) => check(group(g))?,
            Inst::Split { .. } | Inst::Jmp(_) | Inst::Start { .. } | Inst::End { .. } | Inst::Match => {}
            Inst::LoopInit(lp) => {
                let l = lp as usize;
                check(l < b.loops.len() && !loop_seen[l])?;
                loop_seen[l] = true;
                let Some(&Inst::LoopHead { lp: lp2, exit }) = b.insts.get(pc + 1) else { return Err(BAD) };
                check(lp2 == lp && b.insts.get(pc + 2) == Some(&Inst::LoopBegin(lp)))?;
                let tail = (exit as usize).checked_sub(1).ok_or(BAD)?;
                check(tail > pc + 2 && tail < n && tail < regions[top].end)?;
                check(b.insts[tail] == Inst::LoopTail { lp, head: pc as u32 + 1 })?;
                here = regions.len();
                regions.push(Region { kind: Kind::Loop(lp), parent: top, start: pc, end: tail, look: regions[top].look });
                open.push(here);
            }
            Inst::Look { end, .. } => {
                let last = (end as usize).checked_sub(1).ok_or(BAD)?;
                check(last > pc && last < n && last < regions[top].end && b.insts[last] == Inst::LookEnd)?;
                here = regions.len();
                regions.push(Region { kind: Kind::Look, parent: top, start: pc, end: last, look: here });
                dir.resize(regions.len(), None);
                open.push(here);
            }
            Inst::LoopHead { lp, .. } => check(regions[top].kind == Kind::Loop(lp) && pc == regions[top].start + 1)?,
            Inst::LoopBegin(lp) => check(regions[top].kind == Kind::Loop(lp) && pc == regions[top].start + 2)?,
            Inst::LoopTail { lp, .. } => check(regions[top].kind == Kind::Loop(lp) && pc == regions[top].end)?,
            Inst::LookEnd => check(regions[top].kind == Kind::Look && pc == regions[top].end)?,
        }
        region_of[pc] = here;
        if let Some(back) = reads {
            let d = &mut dir[regions[here].look];
            check(d.is_none_or(|d| d == back))?;
            *d = Some(back);
        }
        if pc == regions[here].end {
            open.pop();
        }
    }
    // Control may stay in its region or enter a nested one at its start.
    let enters = |from: usize, to: usize| {
        to < n && {
            let r = region_of[to];
            r == from || (r != 0 && regions[r].parent == from && regions[r].start == to)
        }
    };
    for (pc, &inst) in b.insts.iter().enumerate() {
        let r = region_of[pc];
        let falls = match inst {
            Inst::Jmp(t) => {
                check(t as usize > pc && enters(r, t as usize))?;
                false
            }
            Inst::Split { alt } => {
                check(alt as usize > pc && enters(r, alt as usize))?;
                true
            }
            Inst::LoopHead { exit, .. } => {
                check(enters(regions[r].parent, exit as usize))?;
                true
            }
            Inst::Look { end, .. } => {
                check(enters(regions[r].parent, end as usize))?;
                true
            }
            Inst::Match | Inst::LoopTail { .. } | Inst::LookEnd => false,
            _ => true,
        };
        if falls {
            check(enters(r, pc + 1))?;
        }
    }
    Ok(())
}

/// What the linear matcher relies on: in-range indices and targets, and a
/// thread table of the size its compiler allows.
#[cfg(feature = "linear")]
pub(crate) fn verify_linear(l: &crate::linear::Linear, groups: usize) -> Result<()> {
    use crate::linear::{Inst, MAX_EMPTY_DEPTH, MAX_TABLE};
    let n = l.prog.len();
    let set = |s: u32| (s as usize) < l.sets.len();
    check(n > 0 && l.shift <= MAX_EMPTY_DEPTH && Some(l.nslots) == groups.checked_add(1).and_then(|g| g.checked_mul(2)))?;
    let table = (n << l.shift).checked_mul(l.nslots);
    check(table.is_some_and(|t| t <= MAX_TABLE))?;
    check(l.relevant.len() == n && l.relevant.iter().all(|&r| r >> l.shift == 0))?;
    for (pc, &i) in l.prog.iter().enumerate() {
        let next = pc + 1 < n;
        check(match i {
            Inst::Lit(_) | Inst::Start { .. } | Inst::End { .. } => next,
            Inst::Set(s) | Inst::Word { set: s, .. } => next && set(s),
            Inst::Save(k) => next && (k as usize) < l.nslots,
            Inst::Reset(lo, hi) => next && lo <= hi && hi as usize <= l.nslots,
            Inst::Enter(d) | Inst::Check(d) => next && (d as u32) < l.shift,
            Inst::Split(a, b) => (a as usize) < n && (b as usize) < n,
            Inst::Jmp(t) => (t as usize) < n,
            Inst::Match => true,
        })?;
    }
    if let Some(first) = &l.first {
        check(first.iter().all(|&i| match i {
            Inst::Lit(_) => true,
            Inst::Set(s) => set(s),
            _ => false,
        }))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reason_the_linear_compiler_gives_has_a_code() {
        let src = include_str!("linear.rs");
        let mut found = 0;
        for (i, _) in src.match_indices("Err(\"") {
            let rest = &src[i + 5..];
            let reason = &rest[..rest.find('"').unwrap()];
            assert!(REASONS.contains(&reason), "{reason:?} has no code");
            found += 1;
        }
        assert!(found >= 6);
        let lib = include_str!("lib.rs");
        assert!(lib.contains(&format!("\"{}\"", REASONS[0])));
    }

    #[test]
    fn integers_round_trip_and_overlong_ones_are_rejected() {
        for v in [0, 1, 127, 128, 300, u32::MAX as u64, u64::MAX] {
            let mut w = Writer::default();
            w.uint(v);
            let mut r = Reader { bytes: &w.0, at: 0 };
            assert_eq!(r.uint(), Ok(v));
            assert_eq!(r.at, w.0.len());
        }
        let mut r = Reader { bytes: &[0xFF; 11], at: 0 };
        assert!(r.uint().is_err());
        let mut r = Reader { bytes: &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x02], at: 0 };
        assert!(r.uint().is_err());
    }
}
