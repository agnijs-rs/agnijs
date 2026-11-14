//! A debugger pause request reaches a long native call (TASK-103.33). The
//! native loops and the regex matchers poll for interrupts (TASK-103.28);
//! the same polls take a pending pause request, so `Debugger.pause` stops
//! a `join` over millions of elements or a pathological regex partway
//! instead of after it returns. A regex match is side-effect free, so it
//! stops for the pause and runs again from its start once resumed.
//!
//! The deterministic tests request the pause from the host's interrupt
//! hook, which only native polls ask while the hot call runs: the pause
//! must come at the next poll that reaches the host, not after the call.
//! The cross-thread test requests it from another thread, as the CDP
//! server does, and bounds the time it takes.

use agnijs_engine_api::{Engine, Error, Host, RealmId, Value};
use agnijs_js_debugger::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Asks for a pause the `request_at`-th time the interrupt hook runs, and
/// stops the script once `stop` is set, `deadline` is set from another
/// thread, or the hook has run `give_up` times, so a pause that never
/// comes cannot hang the test.
struct PauseHost {
    requester: RefCell<Option<PauseRequester>>,
    request_at: Cell<u64>,
    asked: Cell<u64>,
    stop: Cell<bool>,
    deadline: Arc<AtomicBool>,
    give_up: Cell<u64>,
}

impl Default for PauseHost {
    fn default() -> PauseHost {
        PauseHost {
            requester: RefCell::new(None),
            request_at: Cell::new(0),
            asked: Cell::new(0),
            stop: Cell::new(false),
            deadline: Arc::default(),
            give_up: Cell::new(GIVE_UP),
        }
    }
}

impl Host for PauseHost {
    fn interrupt(&self) -> bool {
        let n = self.asked.get() + 1;
        self.asked.set(n);
        if n == self.request_at.get() {
            if let Some(r) = &*self.requester.borrow() {
                r.request();
            }
        }
        self.stop.get() || self.deadline.load(Ordering::Relaxed) || n > self.give_up.get()
    }
}

/// More hook calls than any call below makes once paused and resumed.
const GIVE_UP: u64 = 2000;

/// What the client saw at a pause.
#[derive(Clone, Debug)]
struct Seen {
    reason: String,
    function: String,
    line: u32,
    asked: u64,
    at: Instant,
    /// `typeof globalThis.done`, evaluated on the top frame while paused.
    done: Option<Value>,
}

struct Client {
    host: Rc<PauseHost>,
    seen: RefCell<Vec<Seen>>,
    /// Stop the script once paused (a regex that would run for hours).
    stop_on_pause: bool,
}

impl DebugClient for Client {
    fn paused(&self, d: &mut dyn Debuggee, pause: &PauseInfo) -> ResumeAction {
        let top = &pause.frames[0];
        let done = d.evaluate_on_frame(0, "typeof globalThis.done").ok();
        self.seen.borrow_mut().push(Seen {
            reason: format!("{:?}", pause.reason),
            function: top.function_name.clone(),
            line: top.location.line,
            asked: self.host.asked.get(),
            at: Instant::now(),
            done,
        });
        if self.stop_on_pause {
            self.host.stop.set(true);
        }
        ResumeAction::Continue
    }
}

fn debuggee(stop_on_pause: bool) -> (Box<dyn Debuggee>, Rc<PauseHost>, Rc<Client>, RealmId) {
    let host = Rc::new(PauseHost::default());
    let mut d = agnijs_js_vm::make_debuggee(host.clone());
    let client = Rc::new(Client {
        host: host.clone(),
        seen: RefCell::new(Vec::new()),
        stop_on_pause,
    });
    d.attach(client.clone());
    // The requester belongs to the attached session.
    *host.requester.borrow_mut() = Some(d.pause_requester());
    let realm = d.engine().main_realm();
    (d, host, client, realm)
}

/// How long a requested pause may take, generous for debug builds on a
/// busy machine.
const BOUND: Duration = Duration::from_secs(2);

/// Run `setup`, then `hot` with a pause requested at the second time the
/// interrupt hook runs (the first may come from the script's own entry).
/// The pause must come at that poll, inside the call: `done` is not yet
/// set, and the hook has not been asked again.
fn pauses_inside(setup: &str, hot: &str, stop_on_pause: bool) -> (Seen, Result<Value, Error>) {
    let (mut d, host, client, realm) = debuggee(stop_on_pause);
    d.engine()
        .eval_script(realm, "setup.js", setup)
        .unwrap_or_else(|e| panic!("{e}: {setup}"));
    host.asked.set(0);
    host.request_at.set(2);
    let t = Instant::now();
    let r = d.engine().eval_script(realm, "hot.js", hot);
    let seen = client.seen.borrow().clone();
    assert_eq!(seen.len(), 1, "{hot}: pauses {seen:?}");
    let s = seen[0].clone();
    assert_eq!(s.reason, "Requested", "{hot}");
    // Lines count from 0, as CDP's do: the hot call is on the first.
    assert_eq!(s.line, 0, "{hot}: paused at {s:?}");
    assert!(
        s.asked <= 3,
        "{hot}: the pause waited for the call to return: {s:?}"
    );
    assert_eq!(s.done, Some(Value::string("undefined")), "{hot}: {s:?}");
    assert!(s.at - t < BOUND, "{hot}: the pause took {:?}", s.at - t);
    (s, r)
}

#[test]
fn a_pause_request_stops_a_join_over_three_million_elements_partway() {
    let (_, r) = pauses_inside(
        "globalThis.a = Array(3000000)",
        "a.join(','); globalThis.done = true; 'finished'",
        false,
    );
    // Resumed, the join runs to its end.
    assert_eq!(r.unwrap(), Value::string("finished"));
}

#[test]
fn a_pause_request_stops_a_pathological_regex_and_the_match_runs_again() {
    // Lookaround keeps the pattern off the linear-time matcher, so it
    // backtracks with no step budget: hours without the interrupt.
    let (_, r) = pauses_inside(
        "globalThis.s = 'a'.repeat(30)",
        "/(?=(a+)+b)/.test(s); globalThis.done = true",
        true,
    );
    assert!(matches!(r, Err(Error::Interrupted)), "{r:?}");
}

#[test]
fn a_paused_regex_resumes_with_the_same_result() {
    // Small enough to finish, large enough to poll several times.
    let (_, r) = pauses_inside(
        "globalThis.s = 'a'.repeat(18) + 'b'",
        "globalThis.r = [/(?=(a+)+b)/.exec(s)[1], /^(?=(a+)+c)/.test(s)].join(); globalThis.done = true; r",
        false,
    );
    assert_eq!(
        r.unwrap(),
        Value::string(&format!("{},false", "a".repeat(18)))
    );
}

/// The pause from another thread, as `Debugger.pause` arrives from the CDP
/// server's transport thread.
#[test]
fn a_pause_requested_from_another_thread_reaches_a_running_regex() {
    let (mut d, host, client, realm) = debuggee(true);
    d.engine()
        .eval_script(realm, "setup.js", "globalThis.s = 'a'.repeat(30)")
        .unwrap();
    // Only the clock ends this one.
    host.give_up.set(u64::MAX);
    let requester = d.pause_requester();
    let deadline = host.deadline.clone();
    let finished = Arc::new(AtomicBool::new(false));
    let finished2 = finished.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        let at = Instant::now();
        requester.request();
        // Without the pause the regex would run for hours: stop it.
        while !finished2.load(Ordering::Relaxed) && at.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
        deadline.store(true, Ordering::Relaxed);
        at
    });
    let r = d
        .engine()
        .eval_script(realm, "hot.js", "/(?=(a+)+b)/.test(s)");
    finished.store(true, Ordering::Relaxed);
    let at = t.join().unwrap();
    let seen = client.seen.borrow().clone();
    assert!(matches!(r, Err(Error::Interrupted)), "{r:?}");
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(
        seen[0].at - at < BOUND,
        "the pause took {:?}",
        seen[0].at - at
    );
}
