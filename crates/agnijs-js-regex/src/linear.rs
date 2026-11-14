//! The linear-time matcher (ADR 0013 addendum).
//!
//! A Pike VM over UTF-16 input with ECMAScript leftmost-first priority,
//! for patterns without backreferences or lookaround. Every thread runs
//! in lock step over the input and two threads at the same program state
//! and position are merged (the higher-priority one survives), so a match
//! costs O(program states x input length) whatever the pattern.
//!
//! ECMAScript semantics carried over from the backtracking definition:
//!
//! - **Priority.** Threads are kept in the order backtracking would try
//!   them, so the surviving match is the one backtracking finds.
//! - **Capture reset.** Every quantifier iteration starts with the captures
//!   inside the quantified atom unset (RepeatMatcher step 4).
//! - **Empty iterations.** An iteration past the minimum that consumes
//!   nothing fails (RepeatMatcher's continuation). Each thread carries one
//!   "consumed since this iteration began" bit per nesting level of
//!   quantifiers whose body can match empty; the bits are part of the
//!   thread's state for merging, so merging stays sound.
//! - **Counted quantifiers** are unrolled (`a{2,4}` is `aa(?:a(?:a)?)?`).
//!
//! The program is compiled from the same parse tree as the backtracking
//! matcher's (TASK-100.31), so what a character class matches is the same
//! set in both; v-mode classes with strings are alternations there and
//! are eligible like any other.

use crate::charset::SetMatcher;
use crate::parse::{Assertion, Node, Pattern};

/// Largest program, in instructions, after unrolling counted quantifiers.
const MAX_PROGRAM: usize = 20_000;
/// Most nesting levels of quantifiers with a nullable body: each level
/// doubles the number of thread states.
pub(crate) const MAX_EMPTY_DEPTH: u32 = 4;
/// Largest thread table (states x capture slots) the matcher allocates.
pub(crate) const MAX_TABLE: usize = 1 << 18;

const NONE: usize = usize::MAX;

/// Why a pattern is not eligible for the linear matcher.
pub(crate) type Ineligible = &'static str;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Inst {
    Lit(u32),
    Set(u32),
    Split(u32, u32),
    Jmp(u32),
    Save(u32),
    /// Unset capture slots `lo..hi`.
    Reset(u32, u32),
    Start { multiline: bool },
    End { multiline: bool },
    Word { invert: bool, set: u32 },
    /// Begin an iteration of a quantifier with a nullable body at this
    /// nesting level: clear the level's consumed bit.
    Enter(u8),
    /// End such an iteration: fail unless something was consumed.
    Check(u8),
    Match,
}

struct Compiler<'a> {
    prog: Vec<Inst>,
    /// Nesting level of nullable-body iterations at each instruction.
    depth_at: Vec<u8>,
    depth: u8,
    max_depth: u8,
    sets: &'a mut Vec<crate::charset::CharSet>,
}

impl Compiler<'_> {
    fn emit(&mut self, i: Inst) -> Result<usize, Ineligible> {
        if self.prog.len() >= MAX_PROGRAM {
            return Err("program too large");
        }
        self.prog.push(i);
        self.depth_at.push(self.depth);
        Ok(self.prog.len() - 1)
    }

    fn pc(&self) -> u32 {
        self.prog.len() as u32
    }

    fn set(&mut self, s: &crate::charset::CharSet) -> u32 {
        if let Some(i) = self.sets.iter().position(|x| x == s) {
            return i as u32;
        }
        self.sets.push(s.clone());
        self.sets.len() as u32 - 1
    }

    fn node(&mut self, n: &Node) -> Result<(), Ineligible> {
        match n {
            Node::Empty => {}
            Node::Set(s) => {
                let i = match s.single_member() {
                    Some(c) => Inst::Lit(c),
                    None => Inst::Set(self.set(s)),
                };
                self.emit(i)?;
            }
            Node::Assert(a) => {
                let i = match *a {
                    Assertion::Start { multiline } => Inst::Start { multiline },
                    Assertion::End { multiline } => Inst::End { multiline },
                    Assertion::Word { negate, extended } => {
                        Inst::Word { invert: negate, set: self.set(&crate::parse::word_chars(extended)) }
                    }
                };
                self.emit(i)?;
            }
            Node::BackRef { .. } => return Err("backreference"),
            Node::Look { .. } => return Err("lookaround"),
            Node::Capture(k, body) => {
                self.emit(Inst::Save((2 * k) as u32))?;
                self.node(body)?;
                self.emit(Inst::Save((2 * k + 1) as u32))?;
            }
            Node::Concat(v) => {
                for x in v {
                    self.node(x)?;
                }
            }
            Node::Alt(v) => {
                let mut jumps = Vec::new();
                for (k, x) in v.iter().enumerate() {
                    if k + 1 < v.len() {
                        let split = self.emit(Inst::Split(0, 0))?;
                        self.node(x)?;
                        jumps.push(self.emit(Inst::Jmp(0))?);
                        let next = self.pc();
                        self.prog[split] = Inst::Split(split as u32 + 1, next);
                    } else {
                        self.node(x)?;
                    }
                }
                let end = self.pc();
                for j in jumps {
                    self.prog[j] = Inst::Jmp(end);
                }
            }
            Node::Repeat { body, min, max, greedy, first, count } => {
                if *min as usize > MAX_PROGRAM || max.is_some_and(|m| (m - min) as usize > MAX_PROGRAM) {
                    return Err("quantifier bounds too large to unroll");
                }
                let reset = (*count > 0).then(|| Inst::Reset((2 * first) as u32, (2 * (first + count)) as u32));
                for _ in 0..*min {
                    if let Some(r) = reset {
                        self.emit(r)?;
                    }
                    self.node(body)?;
                }
                let check = body.nullable();
                let optional = |c: &mut Self| -> Result<(), Ineligible> {
                    if check {
                        if c.depth as u32 >= MAX_EMPTY_DEPTH {
                            return Err("nullable quantifiers nested too deeply");
                        }
                        c.depth += 1;
                        c.max_depth = c.max_depth.max(c.depth);
                        c.emit(Inst::Enter(c.depth - 1))?;
                    }
                    if let Some(r) = reset {
                        c.emit(r)?;
                    }
                    c.node(body)?;
                    if check {
                        c.emit(Inst::Check(c.depth - 1))?;
                        c.depth -= 1;
                    }
                    Ok(())
                };
                let split = |a: u32, b: u32| if *greedy { Inst::Split(a, b) } else { Inst::Split(b, a) };
                match max {
                    None => {
                        let head = self.emit(Inst::Split(0, 0))?;
                        optional(self)?;
                        self.emit(Inst::Jmp(head as u32))?;
                        let exit = self.pc();
                        self.prog[head] = split(head as u32 + 1, exit);
                    }
                    Some(max) => {
                        let mut splits = Vec::new();
                        for _ in *min..*max {
                            splits.push(self.emit(Inst::Split(0, 0))?);
                            optional(self)?;
                        }
                        let exit = self.pc();
                        for s in splits {
                            self.prog[s] = split(s as u32 + 1, exit);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// Whether the backtracking matcher can be exponential on `n`: some
/// repeated atom can match the same text in more than one way. A repeated
/// alternation whose branches begin with disjoint characters and repeat
/// nothing inside (`(?:[^"\\]|\\.)*`) is not counted: each iteration's
/// branch is decided by its first character.
fn nested(n: &Node) -> bool {
    fn ambiguous(n: &Node) -> bool {
        match n {
            Node::Alt(alts) => {
                if alts.iter().any(|a| a.nullable() || ambiguous(a)) {
                    return true;
                }
                let firsts: Vec<_> = alts.iter().map(first_chars).collect();
                for (i, a) in firsts.iter().enumerate() {
                    let Some(a) = a else { return true };
                    for b in &firsts[i + 1..] {
                        match b {
                            Some(b) if !a.overlaps(b) => {}
                            _ => return true,
                        }
                    }
                }
                false
            }
            Node::Repeat { max, .. } => max.is_none_or(|m| m > 1),
            Node::Capture(_, b) => ambiguous(b),
            Node::Concat(v) => v.iter().any(ambiguous),
            _ => false,
        }
    }
    match n {
        Node::Repeat { body, max, .. } => (max.is_none_or(|m| m > 1) && ambiguous(body)) || nested(body),
        Node::Capture(_, b) => nested(b),
        Node::Concat(v) | Node::Alt(v) => v.iter().any(nested),
        Node::Look { body, .. } => nested(body),
        _ => false,
    }
}

/// The characters `n` must begin with, when it cannot match empty and
/// begins with a plain character.
fn first_chars(n: &Node) -> Option<crate::charset::CharSet> {
    match n {
        Node::Set(s) => Some(s.clone()),
        Node::Capture(_, b) => first_chars(b),
        Node::Concat(v) => v.first().and_then(first_chars),
        _ => None,
    }
}

// ---- thread lists ----------------------------------------------------------------------------

struct Threads {
    dense: Vec<u32>,
    sparse: Vec<u32>,
    len: usize,
    /// `nslots` capture slots per state.
    slots: Vec<usize>,
}

impl Threads {
    fn new(states: usize, nslots: usize) -> Threads {
        Threads { dense: vec![0; states], sparse: vec![0; states], len: 0, slots: vec![NONE; states * nslots] }
    }
    fn contains(&self, id: u32) -> bool {
        let s = self.sparse[id as usize] as usize;
        s < self.len && self.dense[s] == id
    }
    fn insert(&mut self, id: u32) {
        self.sparse[id as usize] = self.len as u32;
        self.dense[self.len] = id;
        self.len += 1;
    }
}

enum Frame {
    Explore(u32, u32),
    Restore(u32, usize),
}

/// A match's capture ranges, the whole match first.
pub(crate) type Ranges = Vec<Option<(usize, usize)>>;

/// Working memory for one regex, reused across matches.
pub(crate) struct Scratch {
    clist: Threads,
    nlist: Threads,
    slots: Vec<usize>,
    stack: Vec<Frame>,
}

// ---- the matcher -----------------------------------------------------------------------------

/// A pattern compiled for the linear-time matcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Linear {
    pub prog: Vec<Inst>,
    pub sets: Vec<SetMatcher>,
    /// log2 of the states per instruction.
    pub shift: u32,
    pub nslots: usize,
    pub unicode: bool,
    /// The backtracking matcher can be exponential on this pattern.
    pub nested: bool,
    /// The instructions that can consume the first character of a match,
    /// when no match can be empty: used to skip ahead between attempts.
    pub first: Option<Vec<Inst>>,
    /// Bits of the consumed mask that matter at each instruction.
    pub relevant: Vec<u32>,
}

/// Whether a code unit is a LineTerminator.
fn is_lt(u: u32) -> bool {
    matches!(u, 0x0A | 0x0D | 0x2028 | 0x2029)
}

impl Linear {
    /// The bytes the program's tables hold (TASK-103.52).
    pub(crate) fn heap_bytes(&self) -> usize {
        use std::mem::size_of_val;
        size_of_val(&*self.prog)
            + self.sets.iter().map(SetMatcher::heap_bytes).sum::<usize>()
            + self.first.as_ref().map_or(0, |f| size_of_val(&**f))
            + size_of_val(&*self.relevant)
    }

    /// Compile the parsed pattern, or say why it is not eligible.
    pub(crate) fn compile(p: &Pattern) -> Result<Linear, Ineligible> {
        let mut sets = Vec::new();
        let mut cmp = Compiler { prog: Vec::new(), depth_at: Vec::new(), depth: 0, max_depth: 0, sets: &mut sets };
        cmp.emit(Inst::Save(0))?;
        cmp.node(&p.node)?;
        cmp.emit(Inst::Save(1))?;
        cmp.emit(Inst::Match)?;
        let shift = cmp.max_depth as u32;
        let nslots = 2 * (p.group_count() + 1);
        let states = cmp.prog.len() << shift;
        if states.saturating_mul(nslots) > MAX_TABLE {
            return Err("thread table too large");
        }
        let relevant = cmp.depth_at.iter().map(|&d| (1u32 << d) - 1).collect();
        let prog = cmp.prog;
        let mut lin = Linear {
            prog,
            sets: sets.iter().map(SetMatcher::new).collect(),
            shift,
            nslots,
            unicode: p.unicode,
            nested: nested(&p.node),
            first: None,
            relevant,
        };
        lin.first = lin.first_set();
        Ok(lin)
    }

    /// Program size, for the routing estimate.
    pub(crate) fn size(&self) -> u64 {
        self.prog.len() as u64
    }

    pub(crate) fn scratch(&self) -> Scratch {
        let states = self.prog.len() << self.shift;
        Scratch {
            clist: Threads::new(states, self.nslots),
            nlist: Threads::new(states, self.nslots),
            slots: vec![NONE; self.nslots],
            stack: Vec::new(),
        }
    }

    /// Epsilon closure of the entry point, ignoring assertions: the
    /// consuming instructions a match can begin with.
    fn first_set(&self) -> Option<Vec<Inst>> {
        let mut seen = vec![false; self.prog.len()];
        let mut stack = vec![0usize];
        let mut out = Vec::new();
        while let Some(pc) = stack.pop() {
            if std::mem::replace(&mut seen[pc], true) {
                continue;
            }
            match self.prog[pc] {
                Inst::Lit(_) | Inst::Set(_) => out.push(self.prog[pc]),
                Inst::Match => return None,
                Inst::Split(a, b) => {
                    stack.push(b as usize);
                    stack.push(a as usize);
                }
                Inst::Jmp(t) => stack.push(t as usize),
                _ => stack.push(pc + 1),
            }
        }
        if out.len() > 8 {
            return None;
        }
        Some(out)
    }

    #[inline]
    fn decode(&self, input: &[u16], pos: usize) -> Option<(u32, usize)> {
        crate::backtrack::decode(input, pos, self.unicode)
    }

    #[inline]
    fn consumes(&self, inst: Inst, ch: u32) -> bool {
        match inst {
            Inst::Lit(x) => x == ch,
            Inst::Set(s) => self.sets[s as usize].contains(ch),
            _ => false,
        }
    }

    /// Find the leftmost match at or after `start` (or exactly at `start`
    /// when `sticky`). Returns the capture slots as ranges, or
    /// `Err(Interrupted)` when `poll`, asked once per
    /// [`SLICE`](crate::backtrack::SLICE) thread steps, says stop.
    pub(crate) fn exec(
        &self,
        s: &mut Scratch,
        input: &[u16],
        start: usize,
        sticky: bool,
        poll: &mut dyn FnMut() -> bool,
    ) -> Result<Option<Ranges>, crate::Interrupted> {
        if start > input.len() {
            return Ok(None);
        }
        // Thread steps since the last poll (TASK-103.28).
        let mut work: u64 = 0;
        s.clist.len = 0;
        s.nlist.len = 0;
        let all = u32::MAX;
        let mut best: Option<Vec<usize>> = None;
        let mut pos = start;
        loop {
            if best.is_none() && (!sticky || pos == start) {
                if s.clist.len == 0 && !sticky {
                    pos = self.skip(input, pos);
                }
                s.slots.iter_mut().for_each(|x| *x = NONE);
                self.add(&mut s.clist, &mut s.slots, &mut s.stack, 0, 0, input, pos);
            }
            if s.clist.len == 0 {
                break;
            }
            work += s.clist.len as u64 + 1;
            if work >= crate::backtrack::SLICE {
                work = 0;
                if poll() {
                    return Err(crate::Interrupted);
                }
            }
            let here = self.decode(input, pos);
            for k in 0..s.clist.len {
                let id = s.clist.dense[k];
                let pc = (id >> self.shift) as usize;
                let inst = self.prog[pc];
                if let Inst::Match = inst {
                    let base = id as usize * self.nslots;
                    best = Some(s.clist.slots[base..base + self.nslots].to_vec());
                    break;
                }
                if let Some((ch, w)) = here {
                    if self.consumes(inst, ch) {
                        let base = id as usize * self.nslots;
                        s.slots.copy_from_slice(&s.clist.slots[base..base + self.nslots]);
                        self.add(&mut s.nlist, &mut s.slots, &mut s.stack, pc + 1, all, input, pos + w);
                    }
                }
            }
            std::mem::swap(&mut s.clist, &mut s.nlist);
            s.nlist.len = 0;
            match here {
                Some((_, w)) => pos += w,
                None => break,
            }
            if sticky && best.is_none() && s.clist.len == 0 {
                break;
            }
        }
        let Some(b) = best else { return Ok(None) };
        Ok(Some(
            (0..self.nslots / 2)
                .map(|g| match (b[2 * g], b[2 * g + 1]) {
                    (x, y) if x != NONE && y != NONE => Some((x, y)),
                    _ => None,
                })
                .collect(),
        ))
    }

    /// With no thread alive, advance to the next position where a match
    /// could begin.
    fn skip(&self, input: &[u16], mut pos: usize) -> usize {
        let Some(first) = &self.first else { return pos };
        while let Some((ch, w)) = self.decode(input, pos) {
            if first.iter().any(|&i| self.consumes(i, ch)) {
                return pos;
            }
            pos += w;
        }
        pos
    }

    /// Add the thread at `pc` and everything it reaches without consuming,
    /// in priority order, to `list`.
    #[allow(clippy::too_many_arguments)]
    fn add(
        &self,
        list: &mut Threads,
        slots: &mut [usize],
        stack: &mut Vec<Frame>,
        pc: usize,
        mask: u32,
        input: &[u16],
        pos: usize,
    ) {
        stack.push(Frame::Explore(pc as u32, mask));
        while let Some(f) = stack.pop() {
            let (mut pc, mut mask) = match f {
                Frame::Restore(slot, v) => {
                    slots[slot as usize] = v;
                    continue;
                }
                Frame::Explore(pc, mask) => (pc as usize, mask),
            };
            loop {
                mask &= self.relevant[pc];
                let id = ((pc as u32) << self.shift) | mask;
                if list.contains(id) {
                    break;
                }
                list.insert(id);
                match self.prog[pc] {
                    Inst::Jmp(t) => pc = t as usize,
                    Inst::Split(a, b) => {
                        stack.push(Frame::Explore(b, mask));
                        pc = a as usize;
                    }
                    Inst::Save(k) => {
                        stack.push(Frame::Restore(k, slots[k as usize]));
                        slots[k as usize] = pos;
                        pc += 1;
                    }
                    Inst::Reset(lo, hi) => {
                        for k in lo..hi {
                            stack.push(Frame::Restore(k, slots[k as usize]));
                            slots[k as usize] = NONE;
                        }
                        pc += 1;
                    }
                    Inst::Start { multiline } => {
                        let ok = pos == 0 || (multiline && is_lt(input[pos - 1] as u32));
                        if !ok {
                            break;
                        }
                        pc += 1;
                    }
                    Inst::End { multiline } => {
                        let ok = pos == input.len() || (multiline && is_lt(input[pos] as u32));
                        if !ok {
                            break;
                        }
                        pc += 1;
                    }
                    Inst::Word { invert, set } => {
                        let w = &self.sets[set as usize];
                        let a = crate::backtrack::decode_left(input, pos, self.unicode).is_some_and(|(c, _)| w.contains(c));
                        let b = self.decode(input, pos).is_some_and(|(c, _)| w.contains(c));
                        if (a != b) == invert {
                            break;
                        }
                        pc += 1;
                    }
                    Inst::Enter(d) => {
                        mask &= !(1 << d);
                        pc += 1;
                    }
                    Inst::Check(d) => {
                        if mask & (1 << d) == 0 {
                            break;
                        }
                        pc += 1;
                    }
                    Inst::Lit(_) | Inst::Set(_) | Inst::Match => {
                        let base = id as usize * self.nslots;
                        list.slots[base..base + self.nslots].copy_from_slice(slots);
                        break;
                    }
                }
            }
        }
    }
}
