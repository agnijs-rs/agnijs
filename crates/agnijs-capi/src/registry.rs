//! Engines and their per-engine state, kept in a thread-local registry.
//!
//! # Threads
//!
//! An engine belongs to the thread that created it: the registry holding it
//! is thread-local, so another thread cannot reach it at all. A global map
//! of owners only tells a caller on the wrong thread why its id is not
//! found ([`AGNIJS_ERR_WRONG_THREAD`] instead of [`AGNIJS_ERR_INVALID_ENGINE`])
//! and holds the interrupt flag that [`crate::agnijs_engine_request_interrupt`]
//! may set from any thread.
//!
//! # The engine lease and reentrancy
//!
//! The engine needs `&mut` access while JavaScript runs, and JavaScript calls
//! host functions, which may call back into the API with the same engine id.
//! Each call that runs the engine *leases* it:
//!
//! - When no call is in progress the engine sits in its slot and the call
//!   takes it out ([`Lease::Owned`]) for its duration.
//! - While JavaScript runs, the engine is out of its slot. A host callback
//!   that receives the engine (a host function, the `import.meta` hook, the
//!   rejection hook) is handed `&mut dyn Engine` by the engine; before
//!   calling into C it pushes a pointer derived from that reference onto
//!   the slot's `lent` stack, and pops it when C returns. A call back into
//!   the API from C leases the top entry ([`Lease::Lent`]), marking it in
//!   use so that only one `&mut` derived from it is live at a time.
//! - Any other call while the engine is out (from a hook that has no engine:
//!   module resolution and loading, console, interrupt, the clock,
//!   finalizers; or a second call while the top entry is in use) gets
//!   [`AGNIJS_ERR_BUSY`]. So does freeing the engine while it is out.
//!
//! The depth of the `lent` stack is capped by `max_reentrancy`, which
//! bounds how much native stack nested host calls can use.

use crate::guard::{drop_payload, panic_message, Fail, Res};
use crate::table::{join, split, Table, GEN_CLASS, GEN_REALM};
use crate::types::*;
use agnijs_engine_api as api;
use agnijs_engine_api::Engine;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::ThreadId;

/// A pointer to an engine lent to a host callback.
struct Lent {
    ptr: *mut (dyn Engine + 'static),
    in_use: bool,
}

/// One engine and everything the host holds in it.
pub struct EngineState {
    pub id: AgnijsEngine,
    pub tag: u16,
    /// The ids the host holds. Declared before `engine` so it is dropped
    /// first.
    pub table: Table,
    /// The last exception, for `agnijs_take_exception`.
    pub exception: Option<api::Exception>,
    /// A value a host function asked to throw (`agnijs_throw`).
    pub pending_throw: Option<api::Value>,
    pub poisoned: bool,
    pub max_reentrancy: u32,
    /// Native stack bytes a call may use (`agnijs_js_parser::stack`).
    pub stack_budget: usize,
    /// Registered host classes: (engine class id, instance finalizer).
    pub classes: Vec<(api::HostClassId, AgnijsFinalizer)>,
    lent: Vec<Lent>,
    engine: Option<Box<dyn Engine>>,
}

impl EngineState {
    pub fn new(id: AgnijsEngine, tag: u16, engine: Box<dyn Engine>, max_reentrancy: u32, stack_budget: usize) -> EngineState {
        EngineState {
            id,
            tag,
            table: Table::new(tag),
            exception: None,
            pending_throw: None,
            poisoned: false,
            max_reentrancy,
            stack_budget,
            classes: Vec::new(),
            lent: Vec::new(),
            engine: Some(engine),
        }
    }

    pub fn realm_id(&self, realm: api::RealmId) -> AgnijsRealm {
        join(self.tag, GEN_REALM, realm.0.saturating_add(1))
    }

    pub fn realm(&self, id: AgnijsRealm) -> Res<api::RealmId> {
        let (tag, generation, index) = split(id);
        if id == 0 || generation != GEN_REALM || index == 0 {
            return Err(Fail::new(AGNIJS_ERR_INVALID_REALM, "not a realm id"));
        }
        if tag != self.tag {
            return Err(Fail::new(AGNIJS_ERR_WRONG_ENGINE, "the realm belongs to another engine"));
        }
        Ok(api::RealmId(index.saturating_sub(1)))
    }

    pub fn class_id(&self, index: usize) -> AgnijsClass {
        join(self.tag, GEN_CLASS, u32::try_from(index).unwrap_or(u32::MAX).saturating_add(1))
    }

    /// The class index behind a class id.
    pub fn class(&self, id: AgnijsClass) -> Res<usize> {
        let (tag, generation, index) = split(id);
        if id == 0 || generation != GEN_CLASS || index == 0 {
            return Err(Fail::new(AGNIJS_ERR_INVALID_HANDLE, "not a class id"));
        }
        if tag != self.tag {
            return Err(Fail::new(AGNIJS_ERR_WRONG_ENGINE, "the class belongs to another engine"));
        }
        let i = usize::try_from(index.saturating_sub(1)).unwrap_or(usize::MAX);
        if i >= self.classes.len() {
            return Err(Fail::new(AGNIJS_ERR_INVALID_HANDLE, "unknown class id"));
        }
        Ok(i)
    }

    /// Whether the engine is out of its slot (a call is running it).
    pub fn busy(&self) -> bool {
        self.engine.is_none() || !self.lent.is_empty()
    }

    fn acquire(&mut self) -> Res<Lease> {
        if let Some(engine) = self.engine.take() {
            return Ok(Lease::Owned(engine));
        }
        let depth = self.lent.len();
        match self.lent.last_mut() {
            Some(top) if !top.in_use => {
                top.in_use = true;
                Ok(Lease::Lent { ptr: top.ptr, depth })
            }
            _ => Err(Fail::new(
                AGNIJS_ERR_BUSY,
                "the engine is running and this callback cannot call into it (only host functions and the \
                 import.meta and rejection hooks may, one call at a time)",
            )),
        }
    }

    fn release(&mut self, lease: Lease) {
        match lease {
            Lease::Owned(engine) => self.engine = Some(engine),
            Lease::Lent { depth, .. } => {
                if let Some(top) = depth.checked_sub(1).and_then(|i| self.lent.get_mut(i)) {
                    top.in_use = false;
                }
            }
        }
    }
}

impl Drop for EngineState {
    fn drop(&mut self) {
        owners_remove(self.id);
        // Values first (they hold engine handles), then the engine. Neither
        // may unwind out of a drop.
        let entries = self.table.clear();
        if let Err(p) = catch_unwind(AssertUnwindSafe(move || drop(entries))) {
            std::mem::forget(p);
        }
        self.exception = None;
        self.pending_throw = None;
        if let Some(engine) = self.engine.take() {
            if let Err(p) = catch_unwind(AssertUnwindSafe(move || drop(engine))) {
                std::mem::forget(p);
            }
        }
    }
}

enum Lease {
    Owned(Box<dyn Engine>),
    Lent { ptr: *mut (dyn Engine + 'static), depth: usize },
}

thread_local! {
    static REGISTRY: RefCell<BTreeMap<AgnijsEngine, EngineState>> = const { RefCell::new(BTreeMap::new()) };
}

struct Owner {
    thread: ThreadId,
    interrupt: Arc<AtomicBool>,
}

static OWNERS: Mutex<BTreeMap<AgnijsEngine, Owner>> = Mutex::new(BTreeMap::new());
static NEXT_ENGINE: AtomicU64 = AtomicU64::new(1);

fn owners() -> MutexGuard<'static, BTreeMap<AgnijsEngine, Owner>> {
    match OWNERS.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn owners_remove(id: AgnijsEngine) {
    owners().remove(&id);
}

/// A fresh engine id (never reused) and its table tag.
pub fn next_engine_id() -> (AgnijsEngine, u16) {
    let id = NEXT_ENGINE.fetch_add(1, Ordering::Relaxed);
    let tag = id.wrapping_sub(1).checked_rem(0xFFFF).unwrap_or(0);
    let tag = u16::try_from(tag).unwrap_or(0).wrapping_add(1);
    (id, tag)
}

/// Register a new engine on this thread.
pub fn register(state: EngineState, interrupt: Arc<AtomicBool>) -> Res<()> {
    let id = state.id;
    owners().insert(id, Owner { thread: std::thread::current().id(), interrupt });
    let mut slot = Some(state);
    let r = REGISTRY.try_with(|r| match r.try_borrow_mut() {
        Ok(mut map) => {
            if let Some(state) = slot.take() {
                map.insert(id, state);
            }
            true
        }
        Err(_) => false,
    });
    // Dropped outside the registry borrow when it could not be stored.
    drop(slot);
    match r {
        Ok(true) => Ok(()),
        Ok(false) => Err(Fail::new(AGNIJS_ERR_BUSY, "the engine registry is in use")),
        Err(_) => {
            owners_remove(id);
            Err(Fail::new(AGNIJS_ERR_INVALID_ENGINE, "this thread is exiting"))
        }
    }
}

fn missing(e: AgnijsEngine) -> Fail {
    if e == 0 {
        return Fail::new(AGNIJS_ERR_INVALID_ENGINE, "the engine id is 0");
    }
    let me = std::thread::current().id();
    match owners().get(&e) {
        Some(o) if o.thread != me => Fail::new(
            AGNIJS_ERR_WRONG_THREAD,
            "the engine belongs to another thread; an engine is used only on the thread that created it",
        ),
        _ => Fail::new(AGNIJS_ERR_INVALID_ENGINE, "unknown or freed engine"),
    }
}

/// Run `f` on the state of engine `e`. `f` must not run JavaScript or call
/// host code. Poisoned engines are refused unless `allow_poisoned`.
pub fn with_state<T>(e: AgnijsEngine, allow_poisoned: bool, f: impl FnOnce(&mut EngineState) -> Res<T>) -> Res<T> {
    REGISTRY
        .try_with(|r| {
            let mut map = r
                .try_borrow_mut()
                .map_err(|_| Fail::new(AGNIJS_ERR_BUSY, "the engine registry is in use (call from a finalizer?)"))?;
            let state = map.get_mut(&e).ok_or_else(|| missing(e))?;
            if state.poisoned && !allow_poisoned {
                return Err(Fail::new(
                    AGNIJS_ERR_POISONED,
                    "a panic inside this engine poisoned it; free it with agnijs_engine_free",
                ));
            }
            f(state)
        })
        .unwrap_or_else(|_| Err(Fail::new(AGNIJS_ERR_INVALID_ENGINE, "this thread is exiting")))
}

/// Remove an engine from the registry and drop it (outside the registry
/// borrow, so finalizers that call back get a clean error).
pub fn unregister(e: AgnijsEngine) -> Res<()> {
    let state = REGISTRY
        .try_with(|r| {
            let mut map = r.try_borrow_mut().map_err(|_| Fail::new(AGNIJS_ERR_BUSY, "the engine registry is in use"))?;
            let state = map.get(&e).ok_or_else(|| missing(e))?;
            if state.busy() {
                return Err(Fail::new(
                    AGNIJS_ERR_BUSY,
                    "the engine is running; free it after the outermost call returns, not from a callback",
                ));
            }
            map.remove(&e).ok_or_else(|| missing(e))
        })
        .unwrap_or_else(|_| Err(Fail::new(AGNIJS_ERR_INVALID_ENGINE, "this thread is exiting")))?;
    drop(state);
    Ok(())
}

/// Ask a running engine to stop, from any thread.
pub fn request_interrupt(e: AgnijsEngine) -> Res<()> {
    match owners().get(&e) {
        Some(o) => {
            o.interrupt.store(true, Ordering::SeqCst);
            Ok(())
        }
        None => Err(Fail::new(AGNIJS_ERR_INVALID_ENGINE, "unknown or freed engine")),
    }
}

/// Run `f` with the engine leased (see the module docs). A panic inside
/// `f` is caught here, poisons the engine and becomes [`AGNIJS_ERR_PANIC`].
pub fn engine_op<T>(e: AgnijsEngine, f: impl FnOnce(&mut dyn Engine) -> Res<T>) -> Res<T> {
    let (mut lease, budget) = with_state(e, false, |st| Ok((st.acquire()?, st.stack_budget)))?;
    let result = catch_unwind(AssertUnwindSafe(|| {
        // Deep recursion (nested source, native reentry) fails with an
        // error once this much stack below here is used, instead of
        // overflowing the host thread's stack. A nested call keeps the
        // outer, tighter limit.
        let _budget = agnijs_js_parser::stack::StackBudget::enter(budget);
        let engine: &mut dyn Engine = match &mut lease {
            Lease::Owned(b) => &mut **b,
            // SAFETY: the pointer was pushed by `lend` from the `&mut dyn
            // Engine` the engine passed to a host callback, and `lend` pops
            // it before that callback's frame returns, so it is valid here
            // (we are inside the C callback that frame is waiting on). The
            // entry is marked in use, so no other lease derives a second
            // `&mut` from it while this one lives.
            Lease::Lent { ptr, .. } => unsafe { &mut **ptr },
        };
        crate::guard::fault::fire_here();
        f(engine)
    }));
    let panicked = result.is_err();
    let returned = REGISTRY.try_with(|r| match r.try_borrow_mut() {
        Ok(mut map) => match map.get_mut(&e) {
            Some(st) => {
                st.release(lease);
                if panicked {
                    st.poisoned = true;
                }
                None
            }
            None => Some(lease),
        },
        Err(_) => Some(lease),
    });
    match returned {
        Ok(None) => {}
        // Nowhere to put the engine back (cannot happen while freeing is
        // refused during a lease); leak it rather than drop it here.
        Ok(Some(lease)) => std::mem::forget(lease),
        Err(_) => {}
    }
    match result {
        Ok(r) => r,
        Err(payload) => {
            let msg = format!("panic inside the engine (it is now poisoned): {}", panic_message(&*payload));
            drop_payload(payload);
            Err(Fail::new(AGNIJS_ERR_PANIC, msg))
        }
    }
}

/// Lend `engine` to host code for the duration of `call` (see the module
/// docs). Fails without calling when the reentrancy limit is reached.
pub fn lend<R>(e: AgnijsEngine, engine: &mut dyn Engine, call: impl FnOnce() -> R) -> api::Result<R> {
    let raw: *mut (dyn Engine + '_) = engine;
    // SAFETY: only the trait object's lifetime bound is erased. The pointer
    // is popped by `Pop` below before this function returns (also when
    // unwinding), so it never outlives the borrow it came from.
    let ptr: *mut (dyn Engine + 'static) = unsafe { std::mem::transmute(raw) };
    with_state(e, true, |st| {
        let max = usize::try_from(st.max_reentrancy).unwrap_or(usize::MAX);
        if st.lent.len() >= max {
            return Err(Fail::new(
                AGNIJS_ERR_LIMIT,
                format!("host callbacks nested more than {max} deep (max_reentrancy)"),
            ));
        }
        st.pending_throw = None;
        st.exception = None;
        st.lent.push(Lent { ptr, in_use: false });
        Ok(())
    })
    .map_err(|f| api::Error::Host(f.message))?;

    struct Pop(AgnijsEngine);
    impl Drop for Pop {
        fn drop(&mut self) {
            let _ = with_state(self.0, true, |st| {
                st.lent.pop();
                Ok(())
            });
        }
    }
    let pop = Pop(e);
    let r = catch_unwind(AssertUnwindSafe(call));
    drop(pop);
    match r {
        Ok(v) => {
            let poisoned = with_state(e, true, |st| Ok(st.poisoned)).unwrap_or(true);
            if poisoned {
                Err(api::Error::Internal("the engine was poisoned by a panic during a host callback".into()))
            } else {
                Ok(v)
            }
        }
        Err(payload) => {
            let msg = format!("host callback panicked: {}", panic_message(&*payload));
            drop_payload(payload);
            Err(api::Error::Host(msg))
        }
    }
}

/// Turn an engine error into a status, keeping a thrown exception for
/// `agnijs_take_exception`.
pub fn engine_error(e: AgnijsEngine, err: api::Error) -> Fail {
    let message = err.to_string();
    let status = match &err {
        api::Error::Exception(_) => AGNIJS_EXCEPTION,
        api::Error::Interrupted => AGNIJS_INTERRUPTED,
        api::Error::OutOfMemory => AGNIJS_OUT_OF_MEMORY,
        api::Error::ForeignHandle => AGNIJS_ERR_WRONG_ENGINE,
        api::Error::UnknownRealm => AGNIJS_ERR_INVALID_REALM,
        api::Error::BytecodeMismatch { .. } | api::Error::BytecodeCorrupt(_) => AGNIJS_ERR_BYTECODE,
        api::Error::Unsupported(_) => AGNIJS_ERR_UNSUPPORTED,
        api::Error::Host(_) => AGNIJS_ERR_HOST,
        api::Error::Internal(_) => AGNIJS_ERR_ENGINE,
    };
    if let api::Error::Exception(ex) = err {
        let _ = with_state(e, true, |st| {
            st.exception = Some(ex);
            Ok(())
        });
    }
    Fail::new(status, message)
}

/// Map an engine result.
pub fn eng<T>(e: AgnijsEngine, r: api::Result<T>) -> Res<T> {
    r.map_err(|err| engine_error(e, err))
}
