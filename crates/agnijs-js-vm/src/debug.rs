//! The engine side of the debugger interface (`agnijs_js_debugger`).
//!
//! Nothing here runs unless a client is attached: `Vm::debug` is `None` and
//! the dispatch loop only tests `Vm::debug_hot` inside the condition it
//! already evaluates for the collector. With a client attached the loop calls
//! [`Vm::debug_check`] before every instruction; it pauses at breakpoint
//! sites (a code block and instruction index), at statement starts while
//! stepping, and when a pause was requested.
//!
//! Scripts are identified by their shared `SourceText`: every code block of
//! one compilation points at the same one, so a frame's script is found from
//! any of its functions, including code compiled before the attach.

use crate::env::{DeclEnv, Environment, ExtraBinding};
use crate::interp::Frame;
use crate::object::Class;
use crate::value::{EnvId, ObjId, PropKey, PropSlot, Value};
use crate::vm::{Fault, JsResult, Vm};
use agnijs_engine_api::{self as api, ConsoleLevel, Engine, RealmId};
use agnijs_js_compiler::bytecode::{BindingKind, CodeBlock, ScopeKind as CScopeKind, SourceText};
use agnijs_js_debugger::{
    self as dbg, AsyncStackTrace, BreakpointId, BreakpointSpec, BreakpointTarget, DebugClient,
    DebugError, Debuggee, ExceptionEvent, FrameInfo, Location, ObjectProperties,
    PauseOnExceptions, PauseReason, PauseRequester, PropertyInfo, PropertyName, ResumeAction,
    ScopeContents, ScopeInfo, ScopeKind, ScriptId, ScriptInfo, ScriptKind, StackFrameInfo,
    Subtype, ValueDescription, ValueKind,
};
use hashbrown::HashMap;
use indexmap::IndexMap;
use std::rc::Rc;

/// Async stack segments kept per chain; older segments are dropped.
const MAX_ASYNC_SEGMENTS: usize = 8;

/// A script the engine keeps by name.
pub struct ScriptEntry {
    pub code: Rc<CodeBlock>,
    pub kind: ScriptKind,
    pub realm: RealmId,
}

struct ScriptRecord {
    info: ScriptInfo,
    /// The root code block (or, for code met before the attach, the first
    /// block seen), keeping the whole tree alive for breakpoint resolution.
    code: Rc<CodeBlock>,
    source: Rc<SourceText>,
    /// Byte offset of each line start.
    line_starts: Vec<usize>,
}

impl ScriptRecord {
    fn new(id: ScriptId, code: Rc<CodeBlock>, kind: ScriptKind, realm: RealmId) -> ScriptRecord {
        let source = code.source.clone();
        let text: &str = &source.text;
        let mut line_starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        let last = *line_starts.last().unwrap();
        let info = ScriptInfo {
            id,
            url: source.name.to_string(),
            kind,
            realm,
            end_line: (line_starts.len() - 1) as u32,
            end_column: text[last..].encode_utf16().count() as u32,
            source_map_url: dbg::source_mapping_url(text),
        };
        ScriptRecord {
            info,
            code,
            source,
            line_starts,
        }
    }

    fn text(&self) -> &str {
        &self.source.text
    }

    /// (line, UTF-16 column) of a byte offset.
    fn position(&self, offset: usize) -> (u32, u32) {
        let offset = offset.min(self.text().len());
        let line = self.line_starts.partition_point(|&s| s <= offset) - 1;
        let start = self.line_starts[line];
        let col = self
            .text()
            .get(start..offset)
            .map(|s| s.encode_utf16().count())
            .unwrap_or(0);
        (line as u32, col as u32)
    }

    /// Byte offset of (line, UTF-16 column), clamped to the line.
    fn offset(&self, line: u32, column: u32) -> Option<usize> {
        let start = *self.line_starts.get(line as usize)?;
        let end = self
            .line_starts
            .get(line as usize + 1)
            .copied()
            .unwrap_or(self.text().len());
        let mut units = 0u32;
        for (i, ch) in self.text()[start..end].char_indices() {
            if units >= column {
                return Some(start + i);
            }
            units += ch.len_utf16() as u32;
        }
        Some(end)
    }

    /// Every code block of the script.
    fn blocks(&self) -> Vec<Rc<CodeBlock>> {
        let mut out = Vec::new();
        let mut stack = vec![self.code.clone()];
        while let Some(c) = stack.pop() {
            stack.extend(c.functions.iter().cloned());
            out.push(c);
        }
        out
    }
}

struct Breakpoint {
    id: BreakpointId,
    spec: BreakpointSpec,
    sites: Vec<(usize, u32)>,
}

/// Debugger state, present only while a client is attached.
pub struct DebugState {
    client: Rc<dyn DebugClient>,
    scripts: Vec<ScriptRecord>,
    by_source: HashMap<usize, ScriptId>,
    breakpoints: Vec<Breakpoint>,
    /// (code block address, instruction index) to the breakpoints there.
    sites: HashMap<(usize, u32), Vec<BreakpointId>>,
    next_breakpoint: u32,
    breakpoints_active: bool,
    /// The step in progress and the frame depth it started at.
    step: Option<(ResumeAction, usize)>,
    pause_on_exceptions: PauseOnExceptions,
    requester: PauseRequester,
    /// Inside `DebugClient::paused`: no nested pauses, frames available.
    in_pause: bool,
    paused_frames: Option<Vec<FrameInfo>>,
    async_depth: u32,
    /// The async stack of the job running now.
    pub(crate) current_async: Option<Rc<AsyncStackTrace>>,
    /// Set by `await` just before it registers its reactions.
    pub(crate) next_async_description: Option<&'static str>,
    /// The exception in flight and the stack where it was thrown.
    exception: Option<(Value, Vec<StackFrameInfo>)>,
}

impl DebugState {
    /// Heap values the debugger holds outside handles (for the collector).
    pub(crate) fn roots(&self) -> Vec<Value> {
        self.exception.iter().map(|(v, _)| *v).collect()
    }
}

impl Vm {
    fn dbg(&mut self) -> Option<&mut DebugState> {
        self.debug.as_deref_mut()
    }

    // ---- scripts ----------------------------------------------------------------------------

    /// Record a named script or source module (for `Engine::script_source`
    /// and for a debugger attached later) and tell an attached debugger.
    pub(crate) fn note_script(&mut self, code: &Rc<CodeBlock>, kind: ScriptKind, realm: RealmId) {
        self.scripts.insert(
            code.source.name.to_string(),
            ScriptEntry {
                code: code.clone(),
                kind,
                realm,
            },
        );
        if self.debug.is_some() {
            self.register_script(code.clone(), kind, realm);
        }
    }

    /// Note a freshly compiled `eval` or `Function` body. Cheap when no
    /// debugger is attached.
    pub(crate) fn debug_script_compiled(&mut self, code: &Rc<CodeBlock>, kind: ScriptKind) {
        if self.debug.is_none() {
            return;
        }
        let realm = self.current_realm;
        self.register_script(code.clone(), kind, realm);
    }

    fn register_script(&mut self, code: Rc<CodeBlock>, kind: ScriptKind, realm: RealmId) -> ScriptId {
        let key = Rc::as_ptr(&code.source) as usize;
        let d = self.debug.as_deref_mut().expect("debugger attached");
        if let Some(id) = d.by_source.get(&key) {
            return *id;
        }
        let id = ScriptId(d.scripts.len() as u32);
        d.scripts.push(ScriptRecord::new(id, code, kind, realm));
        d.by_source.insert(key, id);
        // URL breakpoints set earlier apply to the new script.
        let url = d.scripts[id.0 as usize].info.url.clone();
        let pending: Vec<usize> = (0..d.breakpoints.len())
            .filter(|&i| d.breakpoints[i].spec.target == BreakpointTarget::Url(url.clone()))
            .collect();
        for i in pending {
            let spec = d.breakpoints[i].spec.clone();
            let bid = d.breakpoints[i].id;
            if let Some((_, sites)) = resolve(&d.scripts[id.0 as usize], &spec) {
                for s in &sites {
                    d.sites.entry(*s).or_default().push(bid);
                }
                d.breakpoints[i].sites.extend(sites);
            }
        }
        let info = d.scripts[id.0 as usize].info.clone();
        let client = d.client.clone();
        client.script_parsed(self, &info);
        id
    }

    /// The script of a code block, registering it when first met.
    fn script_of(&mut self, code: &Rc<CodeBlock>) -> ScriptId {
        let key = Rc::as_ptr(&code.source) as usize;
        if let Some(id) = self.debug.as_deref().and_then(|d| d.by_source.get(&key)) {
            return *id;
        }
        let kind = match code.kind {
            agnijs_js_compiler::bytecode::CodeKind::Module => ScriptKind::Module,
            agnijs_js_compiler::bytecode::CodeKind::Script => ScriptKind::Script,
            _ => ScriptKind::Eval,
        };
        let realm = self.current_realm;
        self.register_script(code.clone(), kind, realm)
    }

    fn location_of(&mut self, code: &Rc<CodeBlock>, pc: usize) -> Location {
        let script = self.script_of(code);
        let d = self.debug.as_deref().unwrap();
        let (line, column) = d.scripts[script.0 as usize].position(source_offset(code, pc));
        Location {
            script,
            line,
            column,
        }
    }

    // ---- the dispatch-loop hook -------------------------------------------------------------

    /// Called before instruction `frames[fi].pc` runs while a client is
    /// attached.
    #[cold]
    #[inline(never)]
    pub(crate) fn debug_check(&mut self, fi: usize) {
        let depth = self.frames.len();
        let Some(d) = self.debug.as_deref_mut() else {
            return;
        };
        if d.in_pause {
            return;
        }
        let frame = &self.frames[fi];
        let pc = frame.pc;
        let mut reason = None;
        if d.requester.take() {
            reason = Some(PauseReason::Requested);
        } else if d.breakpoints_active && !d.sites.is_empty() {
            let key = (Rc::as_ptr(&frame.code) as usize, pc as u32);
            if let Some(ids) = d.sites.get(&key) {
                reason = Some(PauseReason::Breakpoint(ids.clone()));
            }
        }
        let stepping = match d.step {
            Some((action, from)) if frame.code.debug.is_statement(pc) => match action {
                ResumeAction::StepInto => true,
                ResumeAction::StepOver => depth <= from,
                ResumeAction::StepOut => depth < from,
                ResumeAction::Continue => false,
            },
            _ => false,
        };
        if let Some(PauseReason::Breakpoint(ids)) = &reason {
            let ids = ids.clone();
            let hit = self.breakpoints_passing_conditions(&ids);
            reason = if hit.is_empty() {
                None
            } else {
                Some(PauseReason::Breakpoint(hit))
            };
        }
        let reason = match (reason, stepping) {
            (Some(r), _) => r,
            (None, true) => PauseReason::Step,
            (None, false) => return,
        };
        self.debug_pause(reason, true);
    }

    fn breakpoints_passing_conditions(&mut self, ids: &[BreakpointId]) -> Vec<BreakpointId> {
        let mut hit = Vec::new();
        for id in ids {
            let cond = self
                .debug
                .as_deref()
                .and_then(|d| d.breakpoints.iter().find(|b| b.id == *id))
                .map(|b| b.spec.condition.clone());
            let Some(cond) = cond else { continue };
            let pass = match cond {
                None => true,
                Some(c) => {
                    self.dbg().unwrap().in_pause = true;
                    let fi = self.frames.len() - 1;
                    let r = self.eval_in_frame(fi, true, &c);
                    if let Some(d) = self.dbg() {
                        d.in_pause = false;
                    }
                    matches!(r, Ok(v) if self.to_boolean(v))
                }
            };
            if pass {
                hit.push(*id);
            }
        }
        hit
    }

    /// A `debugger` statement ran (its instruction has been consumed).
    pub(crate) fn debug_statement(&mut self) {
        if self.debug.as_deref().map(|d| !d.in_pause).unwrap_or(false) {
            self.debug_pause(PauseReason::DebuggerStatement, false);
        }
    }

    /// Pause and run the client. `top_at_pc`: the innermost frame's `pc`
    /// is the instruction about to run (otherwise the one just run).
    fn debug_pause(&mut self, reason: PauseReason, top_at_pc: bool) {
        let frames = self.collect_frames(top_at_pc);
        let depth = self.frames.len();
        let d = self.dbg().unwrap();
        d.in_pause = true;
        d.step = None;
        d.paused_frames = Some(frames.clone());
        let async_stack = d.current_async.clone();
        let client = d.client.clone();
        let info = dbg::PauseInfo {
            reason,
            frames,
            async_stack,
        };
        let action = client.paused(self, &info);
        if let Some(d) = self.dbg() {
            d.in_pause = false;
            d.paused_frames = None;
            d.step = match action {
                ResumeAction::Continue => None,
                a => Some((a, depth)),
            };
        }
    }

    /// A fault is being raised at the top frame (before any unwinding).
    pub(crate) fn debug_fault(&mut self, fault: &Fault) {
        let Fault::Throw(v) = *fault else { return };
        let Some(d) = self.debug.as_deref() else { return };
        if d.in_pause || d.exception.as_ref().map(|(e, _)| *e == v).unwrap_or(false) {
            return;
        }
        let stack = self.capture_stack(64, false);
        let d = self.dbg().unwrap();
        d.exception = Some((v, stack));
        let mode = d.pause_on_exceptions;
        if mode == PauseOnExceptions::None {
            return;
        }
        let uncaught = !self.frames.iter().any(|f| {
            f.code
                .handler_for(f.pc.saturating_sub(1))
                .is_some()
        });
        if mode == PauseOnExceptions::All || uncaught {
            let value = self.to_api_value(v);
            self.debug_pause(PauseReason::Exception { value, uncaught }, false);
        }
    }

    /// The outermost entry into the engine returned: a step in progress
    /// ends, and an exception that reached the host is reported.
    pub(crate) fn debug_run_ended(&mut self, error: Option<&api::Error>) {
        let Some(d) = self.dbg() else { return };
        d.step = None;
        if let Some(e) = error {
            self.debug_exception_escaped(e);
        } else {
            d.exception = None;
        }
    }

    fn debug_exception_escaped(&mut self, e: &api::Error) {
        let Some(d) = self.dbg() else { return };
        let captured = d.exception.take();
        let api::Error::Exception(ex) = e else { return };
        let stack = match captured {
            Some((v, stack)) if self.from_api_value(&ex.value).ok() == Some(v) => stack,
            _ => Vec::new(),
        };
        let text = if ex.name.is_empty() {
            format!("Uncaught {}", ex.message)
        } else {
            format!("Uncaught {}: {}", ex.name, ex.message)
        };
        let event = ExceptionEvent {
            value: ex.value.clone(),
            text,
            stack,
        };
        let client = self.dbg().unwrap().client.clone();
        client.exception_thrown(self, &event);
    }

    /// A `console` call, before the host sees it.
    pub(crate) fn debug_console(&mut self, level: ConsoleLevel, args: &[Value]) {
        let Some(d) = self.debug.as_deref() else { return };
        let client = d.client.clone();
        let args: Vec<api::Value> = args.iter().map(|v| self.to_api_value(*v)).collect();
        let realm = self.current_realm;
        client.console(self, realm, level, &args);
    }

    /// An `await` is about to register its reactions.
    pub(crate) fn debug_note_await(&mut self) {
        if let Some(d) = self.dbg() {
            d.next_async_description = Some("await");
        }
    }

    /// The async stack to attach to a promise reaction being registered now.
    pub(crate) fn debug_async_parent(&mut self) -> Option<Rc<AsyncStackTrace>> {
        let d = self.debug.as_deref_mut()?;
        if d.async_depth == 0 {
            return None;
        }
        let description = d.next_async_description.take().unwrap_or("Promise.then");
        let limit = d.async_depth as usize;
        let frames = self.capture_stack(limit, false);
        let d = self.debug.as_deref().unwrap();
        if frames.is_empty() {
            return d.current_async.clone();
        }
        let mut parent = d.current_async.clone();
        let mut n = 0;
        let mut p = parent.as_deref();
        while let Some(s) = p {
            n += 1;
            p = s.parent.as_deref();
        }
        if n >= MAX_ASYNC_SEGMENTS {
            parent = None;
        }
        Some(Rc::new(AsyncStackTrace {
            description: description.to_string(),
            frames,
            parent,
        }))
    }

    /// Make `trace` the current async stack; returns the previous one.
    pub(crate) fn debug_enter_async(
        &mut self,
        trace: Option<Rc<AsyncStackTrace>>,
    ) -> Option<Rc<AsyncStackTrace>> {
        match self.dbg() {
            Some(d) => std::mem::replace(&mut d.current_async, trace),
            None => None,
        }
    }

    // ---- frames and scopes ------------------------------------------------------------------

    fn frame_pc(&self, fi: usize, top_at_pc: bool) -> usize {
        let f = &self.frames[fi];
        if fi + 1 == self.frames.len() && top_at_pc {
            f.pc
        } else {
            f.pc.saturating_sub(1)
        }
    }

    fn capture_stack(&mut self, limit: usize, top_at_pc: bool) -> Vec<StackFrameInfo> {
        let mut out = Vec::new();
        for fi in (0..self.frames.len()).rev() {
            if out.len() >= limit {
                break;
            }
            let pc = self.frame_pc(fi, top_at_pc);
            let code = self.frames[fi].code.clone();
            let loc = self.location_of(&code, pc);
            out.push(StackFrameInfo {
                function_name: function_name(&self.frames[fi]),
                script: Some(loc.script),
                url: code.source.name.to_string(),
                line: loc.line,
                column: loc.column,
            });
        }
        out
    }

    fn collect_frames(&mut self, top_at_pc: bool) -> Vec<FrameInfo> {
        let mut out = Vec::new();
        for fi in (0..self.frames.len()).rev() {
            let pc = self.frame_pc(fi, top_at_pc);
            let code = self.frames[fi].code.clone();
            let location = self.location_of(&code, pc);
            let this = self.frames[fi].this;
            let this = self.to_api_value(this);
            let scopes = self.frame_scopes(fi, pc);
            out.push(FrameInfo {
                index: out.len(),
                function_name: function_name(&self.frames[fi]),
                location,
                url: code.source.name.to_string(),
                this,
                scopes,
            });
        }
        out
    }

    /// Register bindings live at `pc` in frame `fi`, inner declarations
    /// first.
    fn live_locals(&self, fi: usize, pc: usize) -> Vec<(Rc<str>, u16)> {
        let code = &self.frames[fi].code;
        let mut seen: Vec<Rc<str>> = Vec::new();
        let mut out = Vec::new();
        for l in code.debug.locals.iter().rev() {
            if (l.start as usize) <= pc && pc < l.end as usize && !seen.contains(&l.name) {
                seen.push(l.name.clone());
                out.push((l.name.clone(), l.reg));
            }
        }
        out
    }

    fn frame_scopes(&mut self, fi: usize, pc: usize) -> Vec<ScopeInfo> {
        let mut local: Vec<(String, Value)> = Vec::new();
        let base = self.frames[fi].base;
        for (name, reg) in self.live_locals(fi, pc) {
            let v = self.regs[base + reg as usize];
            if visible(&name) && !v.is_empty() {
                local.push((name.to_string(), v));
            }
        }
        let mut scopes = Vec::new();
        let stop = self.frames[fi].base_env;
        let mut cur = self.frames[fi].env;
        // The frame's own environments join the local scope.
        while let Some(e) = cur {
            if Some(e) == stop {
                break;
            }
            match self.heap.env(e) {
                Environment::Declarative(_) => {
                    for (n, v) in self.env_bindings(e) {
                        if !local.iter().any(|(m, _)| *m == n) {
                            local.push((n, v));
                        }
                    }
                }
                _ => break,
            }
            cur = self.heap.env(e).parent();
        }
        let local = local
            .into_iter()
            .map(|(n, v)| (n, self.to_api_value(v)))
            .collect();
        scopes.push(ScopeInfo {
            kind: ScopeKind::Local,
            contents: ScopeContents::Variables(local),
        });
        while let Some(e) = cur {
            match self.heap.env(e) {
                Environment::Declarative(d) => {
                    let kind = match d.scope.as_ref().map(|(c, i)| c.scopes[*i as usize].kind) {
                        Some(CScopeKind::Module) => ScopeKind::Module,
                        Some(CScopeKind::Catch) => ScopeKind::Catch,
                        Some(CScopeKind::Eval) | None => ScopeKind::Eval,
                        _ => ScopeKind::Closure,
                    };
                    let vars = self
                        .env_bindings(e)
                        .into_iter()
                        .map(|(n, v)| (n, self.to_api_value(v)))
                        .collect::<Vec<_>>();
                    if !vars.is_empty() || kind == ScopeKind::Module {
                        scopes.push(ScopeInfo {
                            kind,
                            contents: ScopeContents::Variables(vars),
                        });
                    }
                }
                Environment::Object(o) => {
                    let (obj, with) = (o.object, o.with);
                    let v = self.to_api_value(Value::Object(obj));
                    scopes.push(ScopeInfo {
                        kind: if with { ScopeKind::With } else { ScopeKind::Global },
                        contents: ScopeContents::Object(v),
                    });
                }
                Environment::Global(r) => {
                    let r = *r;
                    let lex: Vec<(String, Value)> = self.realms[r.0 as usize]
                        .global_lexicals
                        .iter()
                        .filter(|(_, b)| !b.value.is_empty())
                        .map(|(n, b)| (n.to_string(), b.value))
                        .collect();
                    if !lex.is_empty() {
                        let vars = lex
                            .into_iter()
                            .map(|(n, v)| (n, self.to_api_value(v)))
                            .collect();
                        scopes.push(ScopeInfo {
                            kind: ScopeKind::Script,
                            contents: ScopeContents::Variables(vars),
                        });
                    }
                    let g = self.realms[r.0 as usize].global;
                    let v = self.to_api_value(Value::Object(g));
                    scopes.push(ScopeInfo {
                        kind: ScopeKind::Global,
                        contents: ScopeContents::Object(v),
                    });
                }
            }
            cur = self.heap.env(e).parent();
        }
        scopes
    }

    /// Initialised, user-visible bindings of a declarative environment.
    fn env_bindings(&self, e: EnvId) -> Vec<(String, Value)> {
        let mut out = Vec::new();
        if let Environment::Declarative(d) = self.heap.env(e) {
            if let Some((code, idx)) = &d.scope {
                for (i, (n, _)) in code.scopes[*idx as usize].bindings.iter().enumerate() {
                    let v = d.slots[i];
                    if visible(n) && !v.is_empty() {
                        out.push((n.to_string(), v));
                    }
                }
            }
            if let Some(extra) = &d.extra {
                for (n, b) in extra.iter() {
                    if visible(n) && !b.value.is_empty() {
                        out.push((n.to_string(), b.value));
                    }
                }
            }
        }
        out
    }

    /// Evaluate `source` as a direct `eval` in frame `fi`, with the frame's
    /// register bindings reachable by name (and written back afterwards).
    fn eval_in_frame(&mut self, fi: usize, top_at_pc: bool, source: &str) -> JsResult<Value> {
        let pc = self.frame_pc(fi, top_at_pc);
        let base = self.frames[fi].base;
        let locals = self.live_locals(fi, pc);
        let mut extra = IndexMap::new();
        for (name, reg) in &locals {
            extra.insert(
                name.clone(),
                ExtraBinding {
                    value: self.regs[base + *reg as usize],
                    kind: BindingKind::Let,
                    deletable: false,
                },
            );
        }
        let realm = self.frames[fi].realm;
        let parent = self.frames[fi]
            .env
            .or(Some(self.realm_of(realm).global_env));
        let env = self.heap.alloc_env(Environment::Declarative(DeclEnv {
            slots: Vec::new(),
            scope: None,
            extra: Some(Box::new(extra)),
            parent,
            is_var_scope: false,
        }));
        let strict = self.frames[fi].code.strict;
        let options = self.eval_options(fi, strict);
        let code = match agnijs_js_compiler::compile_eval(source, "<debugger>", &options) {
            Ok(c) => c,
            Err(e) => return Err(self.throw_syntax(&e.message)),
        };
        let f = &self.frames[fi];
        let (this, callee, new_target, module) = (f.this, f.callee, f.new_target, f.module);
        let saved_realm = self.current_realm;
        let r = self.run_eval_code(code, Some(env), realm, this, callee, new_target, module);
        self.current_realm = saved_realm;
        if let Environment::Declarative(d) = self.heap.env(env) {
            let extra = d.extra.as_ref().unwrap();
            let back: Vec<(u16, Value)> = locals
                .iter()
                .filter_map(|(n, reg)| extra.get(n).map(|b| (*reg, b.value)))
                .collect();
            for (reg, v) in back {
                self.regs[base + reg as usize] = v;
            }
        }
        r
    }

    // ---- values -----------------------------------------------------------------------------

    fn class_name(&mut self, o: ObjId) -> Option<String> {
        let ctor = self.lookup_data_property(o, PropKey::Str(self.names.constructor))?;
        let Value::Object(c) = ctor else { return None };
        match self.lookup_data_property(c, PropKey::Str(self.names.name)) {
            Some(Value::String(s)) => {
                let s = self.heap.str_to_string(s);
                (!s.is_empty()).then_some(s)
            }
            _ => None,
        }
    }

    fn describe_value(&mut self, v: Value) -> ValueDescription {
        let prim = |kind, description: String| ValueDescription {
            kind,
            subtype: None,
            class_name: None,
            description,
        };
        match v {
            Value::Undefined | Value::Empty => prim(ValueKind::Undefined, "undefined".into()),
            Value::Null => prim(ValueKind::Null, "null".into()),
            Value::Bool(b) => prim(ValueKind::Boolean, b.to_string()),
            Value::Number(_) => {
                let s = self.to_rust_string(v).unwrap_or_default();
                prim(ValueKind::Number, s)
            }
            Value::String(s) => prim(ValueKind::String, self.heap.str_to_string(s)),
            Value::BigInt(_) => {
                let s = self.to_rust_string(v).unwrap_or_default();
                prim(ValueKind::BigInt, format!("{s}n"))
            }
            Value::Symbol(s) => {
                let d = crate::builtins::symbol::symbol_descriptive_string(self, s);
                let d = match d {
                    Value::String(d) => self.heap.str_to_string(d),
                    _ => "Symbol()".into(),
                };
                prim(ValueKind::Symbol, d)
            }
            Value::Object(o) => self.describe_object(o),
        }
    }

    fn describe_object(&mut self, o: ObjId) -> ValueDescription {
        let callable = self.heap.obj(o).is_callable();
        let subtype = match &self.heap.obj(o).class {
            Class::Array(_) => Some(Subtype::Array),
            Class::Error => Some(Subtype::Error),
            Class::Promise(_) => Some(Subtype::Promise),
            Class::RegExp(_) => Some(Subtype::RegExp),
            Class::Date(_) => Some(Subtype::Date),
            Class::Map(_) => Some(Subtype::Map),
            Class::Set(_) => Some(Subtype::Set),
            Class::WeakMap(_) => Some(Subtype::WeakMap),
            Class::WeakSet(_) => Some(Subtype::WeakSet),
            Class::WeakRef(_) => Some(Subtype::WeakRef),
            Class::Proxy(_) => Some(Subtype::Proxy),
            Class::TypedArray(_) => Some(Subtype::TypedArray),
            Class::ArrayBuffer(_) => Some(Subtype::ArrayBuffer),
            Class::DataView(_) => Some(Subtype::DataView),
            Class::Generator(_) => Some(Subtype::Generator),
            Class::ArrayIterator(_)
            | Class::MapIterator(_)
            | Class::SetIterator(_)
            | Class::StringIterator { .. } => Some(Subtype::Iterator),
            Class::Namespace(_) => Some(Subtype::Module),
            _ => None,
        };
        let class_name = self.class_name(o).or_else(|| {
            Some(
                match subtype {
                    Some(Subtype::Array) => "Array",
                    Some(Subtype::Error) => "Error",
                    Some(Subtype::Proxy) => "Proxy",
                    Some(Subtype::Module) => "Module",
                    _ if callable => "Function",
                    _ => "Object",
                }
                .to_string(),
            )
        });
        let cn = class_name.clone().unwrap_or_default();
        let description = if callable {
            self.function_description(o)
        } else {
            match &self.heap.obj(o).class {
                Class::Array(a) => format!("{cn}({})", a.length),
                Class::Error => {
                    match self.lookup_data_property(o, PropKey::Str(self.names.stack)) {
                        Some(Value::String(s)) => self.heap.str_to_string(s),
                        _ => {
                            let (name, message) = self.exception_name_message(Value::Object(o));
                            if message.is_empty() {
                                name
                            } else {
                                format!("{name}: {message}")
                            }
                        }
                    }
                }
                Class::Promise(_) => "Promise".into(),
                _ => cn,
            }
        };
        ValueDescription {
            kind: if callable {
                ValueKind::Function
            } else {
                ValueKind::Object
            },
            subtype,
            class_name,
            description,
        }
    }

    fn function_description(&mut self, o: ObjId) -> String {
        if let Some(f) = self.heap.obj(o).function() {
            let code = f.code.clone();
            if let Some(text) = code
                .source
                .text
                .get(code.span.start as usize..code.span.end as usize)
            {
                if !text.is_empty() {
                    return text.to_string();
                }
            }
        }
        let name = match self.lookup_data_property(o, PropKey::Str(self.names.name)) {
            Some(Value::String(s)) => self.heap.str_to_string(s),
            _ => String::new(),
        };
        format!("function {name}() {{ [native code] }}")
    }

    fn object_properties(&mut self, o: ObjId) -> ObjectProperties {
        let mut out = ObjectProperties::default();
        let is_proxy = matches!(self.heap.obj(o).class, Class::Proxy(_));
        if !is_proxy {
            for key in self.heap.own_keys(o) {
                let Some(p) = self.heap.get_own_property(o, key) else { continue };
                let name = match key {
                    PropKey::Sym(s) => PropertyName::Symbol(self.to_api_value(Value::Symbol(s))),
                    k => {
                        let s = self.key_to_string(k);
                        PropertyName::String(self.heap.str_to_string(s))
                    }
                };
                let (value, get, set) = match p.slot {
                    PropSlot::Data(v) => (Some(self.to_api_value(v)), None, None),
                    PropSlot::Accessor { get, set } => (
                        None,
                        (!get.is_undefined()).then(|| self.to_api_value(get)),
                        (!set.is_undefined()).then(|| self.to_api_value(set)),
                    ),
                };
                out.properties.push(PropertyInfo {
                    name,
                    value,
                    get,
                    set,
                    writable: p.attrs.writable(),
                    enumerable: p.attrs.enumerable(),
                    configurable: p.attrs.configurable(),
                });
            }
        }
        let mut internal: Vec<(&str, Value)> = Vec::new();
        match &self.heap.obj(o).class {
            Class::Boolean(b) => internal.push(("[[PrimitiveValue]]", Value::Bool(*b))),
            Class::Number(n) => internal.push(("[[PrimitiveValue]]", Value::Number(*n))),
            Class::String(s) => internal.push(("[[PrimitiveValue]]", Value::String(*s))),
            Class::Symbol(s) => internal.push(("[[PrimitiveValue]]", Value::Symbol(*s))),
            Class::BigInt(b) => internal.push(("[[PrimitiveValue]]", Value::BigInt(*b))),
            Class::Bound(b) => {
                internal.push(("[[TargetFunction]]", Value::Object(b.target)));
                internal.push(("[[BoundThis]]", b.this));
            }
            Class::Proxy(p) => {
                internal.push(("[[Target]]", p.target.map(Value::Object).unwrap_or(Value::Null)));
                internal.push(("[[Handler]]", p.handler.map(Value::Object).unwrap_or(Value::Null)));
            }
            Class::WeakRef(t) => internal.push(("[[WeakRefTarget]]", *t)),
            _ => {}
        }
        if let Class::Promise(p) = &self.heap.obj(o).class {
            use crate::builtins::promise::PromiseStatus;
            let (state, result) = (p.status, p.result);
            let state = match state {
                PromiseStatus::Pending => "pending",
                PromiseStatus::Fulfilled => "fulfilled",
                PromiseStatus::Rejected => "rejected",
            };
            let s = self.str(state);
            internal.push(("[[PromiseState]]", s));
            if state != "pending" {
                internal.push(("[[PromiseResult]]", result));
            }
        }
        if let Some(proto) = self.heap.obj(o).proto {
            if !is_proxy {
                internal.push(("[[Prototype]]", Value::Object(proto)));
            }
        }
        out.internal = internal
            .into_iter()
            .map(|(n, v)| (n.to_string(), self.to_api_value(v)))
            .collect();
        out
    }
}

/// The source offset reported for instruction `pc`: its statement's start
/// when it begins one.
fn source_offset(code: &CodeBlock, pc: usize) -> usize {
    match code
        .debug
        .statements
        .binary_search_by_key(&(pc as u32), |s| s.pc)
    {
        Ok(i) => code.debug.statements[i].offset as usize,
        Err(_) => code.offset_at(pc) as usize,
    }
}

fn visible(name: &str) -> bool {
    !name.starts_with('*') && !name.starts_with('#')
}

fn function_name(f: &Frame) -> String {
    match f.callee {
        Some(_) => f.code.name.to_string(),
        None => String::new(),
    }
}

/// Resolve a breakpoint in one script: the statement starts at the first
/// breakable offset at or after the requested position.
fn resolve(rec: &ScriptRecord, spec: &BreakpointSpec) -> Option<(usize, Vec<(usize, u32)>)> {
    let target = rec.offset(spec.line, spec.column.unwrap_or(0))?;
    let mut best: Option<usize> = None;
    let mut sites = Vec::new();
    for code in rec.blocks() {
        for s in &code.debug.statements {
            let (pc, off) = (s.pc, s.offset as usize);
            if off < target {
                continue;
            }
            match best {
                Some(b) if off > b => {}
                Some(b) if off == b => sites.push((Rc::as_ptr(&code) as usize, pc)),
                _ => {
                    best = Some(off);
                    sites.clear();
                    sites.push((Rc::as_ptr(&code) as usize, pc));
                }
            }
        }
    }
    best.map(|b| (b, sites))
}

// ---- the interface ----------------------------------------------------------------------------

impl Debuggee for Vm {
    fn engine(&mut self) -> &mut dyn Engine {
        self
    }

    fn attach(&mut self, client: Rc<dyn DebugClient>) {
        self.debug = Some(Box::new(DebugState {
            client,
            scripts: Vec::new(),
            by_source: HashMap::new(),
            breakpoints: Vec::new(),
            sites: HashMap::new(),
            next_breakpoint: 1,
            breakpoints_active: true,
            step: None,
            pause_on_exceptions: PauseOnExceptions::None,
            requester: PauseRequester::new(),
            in_pause: false,
            paused_frames: None,
            async_depth: 0,
            current_async: None,
            next_async_description: None,
            exception: None,
        }));
        self.debug_hot = true;
        // Scripts the engine still holds become known now, oldest first.
        let mut known: Vec<(Rc<CodeBlock>, ScriptKind, RealmId)> = self
            .scripts
            .values()
            .map(|s| (s.code.clone(), s.kind, s.realm))
            .collect();
        known.sort_by_key(|(c, _, _)| c.source.name.to_string());
        for (code, kind, realm) in known {
            self.register_script(code, kind, realm);
        }
    }

    fn detach(&mut self) {
        self.debug = None;
        self.debug_hot = false;
    }

    fn is_attached(&self) -> bool {
        self.debug.is_some()
    }

    fn scripts(&self) -> Vec<ScriptInfo> {
        self.debug
            .as_deref()
            .map(|d| d.scripts.iter().map(|s| s.info.clone()).collect())
            .unwrap_or_default()
    }

    fn script_source(&self, script: ScriptId) -> Option<String> {
        let d = self.debug.as_deref()?;
        d.scripts
            .get(script.0 as usize)
            .map(|s| s.text().to_string())
    }

    fn set_breakpoint(&mut self, spec: BreakpointSpec) -> (BreakpointId, Vec<Location>) {
        let Some(d) = self.dbg() else {
            return (BreakpointId(0), Vec::new());
        };
        let id = BreakpointId(d.next_breakpoint);
        d.next_breakpoint += 1;
        let targets: Vec<usize> = match &spec.target {
            BreakpointTarget::Script(s) => vec![s.0 as usize],
            BreakpointTarget::Url(u) => (0..d.scripts.len())
                .filter(|&i| d.scripts[i].info.url == *u)
                .collect(),
        };
        let mut sites = Vec::new();
        let mut locations = Vec::new();
        for i in targets {
            let Some(rec) = d.scripts.get(i) else { continue };
            if let Some((off, s)) = resolve(rec, &spec) {
                let (line, column) = rec.position(off);
                locations.push(Location {
                    script: rec.info.id,
                    line,
                    column,
                });
                sites.extend(s);
            }
        }
        for s in &sites {
            d.sites.entry(*s).or_default().push(id);
        }
        d.breakpoints.push(Breakpoint { id, spec, sites });
        (id, locations)
    }

    fn remove_breakpoint(&mut self, id: BreakpointId) -> bool {
        let Some(d) = self.dbg() else { return false };
        let Some(i) = d.breakpoints.iter().position(|b| b.id == id) else {
            return false;
        };
        let bp = d.breakpoints.remove(i);
        for s in bp.sites {
            if let Some(ids) = d.sites.get_mut(&s) {
                ids.retain(|x| *x != id);
                if ids.is_empty() {
                    d.sites.remove(&s);
                }
            }
        }
        true
    }

    fn breakable_locations(&self, start: Location, end: Option<Location>) -> Vec<Location> {
        let Some(d) = self.debug.as_deref() else { return Vec::new() };
        let Some(rec) = d.scripts.get(start.script.0 as usize) else {
            return Vec::new();
        };
        let from = rec.offset(start.line, start.column).unwrap_or(usize::MAX);
        let to = end
            .and_then(|e| rec.offset(e.line, e.column))
            .unwrap_or(usize::MAX);
        let mut offs: Vec<usize> = rec
            .blocks()
            .iter()
            .flat_map(|c| {
                c.debug
                    .statements
                    .iter()
                    .map(|s| s.offset as usize)
                    .collect::<Vec<_>>()
            })
            .filter(|&o| o >= from && o < to)
            .collect();
        offs.sort_unstable();
        offs.dedup();
        offs.into_iter()
            .map(|o| {
                let (line, column) = rec.position(o);
                Location {
                    script: start.script,
                    line,
                    column,
                }
            })
            .collect()
    }

    fn set_breakpoints_active(&mut self, active: bool) {
        if let Some(d) = self.dbg() {
            d.breakpoints_active = active;
        }
    }

    fn set_pause_on_exceptions(&mut self, mode: PauseOnExceptions) {
        if let Some(d) = self.dbg() {
            d.pause_on_exceptions = mode;
        }
    }

    fn set_async_stack_depth(&mut self, depth: u32) {
        if let Some(d) = self.dbg() {
            d.async_depth = depth;
            if depth == 0 {
                d.current_async = None;
            }
        }
    }

    fn pause_requester(&self) -> PauseRequester {
        self.debug
            .as_deref()
            .map(|d| d.requester.clone())
            .unwrap_or_default()
    }

    fn describe(&mut self, value: &api::Value) -> ValueDescription {
        match self.from_api_value(value) {
            Ok(v) => self.describe_value(v),
            Err(_) => ValueDescription {
                kind: ValueKind::Undefined,
                subtype: None,
                class_name: None,
                description: "<released>".into(),
            },
        }
    }

    fn properties(&mut self, object: &api::Value) -> Result<ObjectProperties, DebugError> {
        match self.from_api_value(object)? {
            Value::Object(o) => Ok(self.object_properties(o)),
            _ => Err(DebugError::NotAnObject),
        }
    }

    fn evaluate_on_frame(
        &mut self,
        frame: usize,
        expression: &str,
    ) -> Result<api::Value, DebugError> {
        let Some(d) = self.debug.as_deref() else {
            return Err(DebugError::NotPaused);
        };
        let Some(frames) = &d.paused_frames else {
            return Err(DebugError::NotPaused);
        };
        if frame >= frames.len() || frame >= self.frames.len() {
            return Err(DebugError::NoSuchFrame);
        }
        // The pause's top frame: was its pc the next or the last instruction?
        let top_at_pc = {
            let top = &frames[0];
            let fi = self.frames.len() - 1;
            let code = self.frames[fi].code.clone();
            let here = self.frames[fi].pc;
            let key = Rc::as_ptr(&code.source) as usize;
            let rec = &d.scripts[d.by_source[&key].0 as usize];
            rec.position(source_offset(&code, here)) == (top.location.line, top.location.column)
        };
        let fi = self.frames.len() - 1 - frame;
        self.handles.reclaim();
        let r = self.in_native(|vm| vm.eval_in_frame(fi, top_at_pc, expression));
        match r {
            Ok(v) => Ok(self.to_api_value(v)),
            Err(f) => {
                if let Some(d) = self.dbg() {
                    d.exception = None;
                }
                Err(DebugError::Engine(self.fault_to_error(f)))
            }
        }
    }

    fn paused_frames(&mut self) -> Result<Vec<FrameInfo>, DebugError> {
        self.debug
            .as_deref()
            .and_then(|d| d.paused_frames.clone())
            .ok_or(DebugError::NotPaused)
    }
}

/// Create an engine with the debugger interface (the same engine as
/// [`crate::make_engine`]).
pub fn make_debuggee(host: Rc<dyn api::Host>) -> Box<dyn Debuggee> {
    let mut vm = Vm::new(host);
    Vm::create_realm(&mut vm);
    Box::new(vm)
}
