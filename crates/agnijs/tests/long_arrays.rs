//! Converting a script's array to Rust data must not size an allocation by
//! the array's `length` (TASK-103.39). A sparse array costs the engine
//! nothing at `length` 2^32-1, but preallocating that many slots aborted
//! the process. Conversion now refuses more than
//! `agnijs::MAX_CONVERTED_VALUES` values with an error.
//!
//! A red run of these tests aborts rather than fails.

use agnijs::{Context, Error, MAX_CONVERTED_VALUES};
use std::time::{Duration, Instant};

const BOUND: Duration = Duration::from_secs(5);

// The cap leaves room for ordinary data.
const _: () = assert!(MAX_CONVERTED_VALUES >= 1 << 20);

const HUGE: [&str; 3] = [
    "(() => { const a = []; a.length = 4294967295; return a; })()",
    "Object.assign([], { length: 2 ** 32 - 1 })",
    // Past the cap, nested one level down.
    "[1, (() => { const a = []; a.length = 2 ** 23; return a; })()]",
];

#[test]
fn eval_of_a_huge_sparse_array_is_a_conversion_error() {
    let mut js = Context::new();
    for src in HUGE {
        let t = Instant::now();
        let e = js.eval::<serde_json::Value>(src).unwrap_err();
        assert!(matches!(e, Error::Convert(_)), "{src}: {e}");
        assert!(t.elapsed() < BOUND, "{src}: took {:?}", t.elapsed());
    }
    // The context carries on.
    assert_eq!(js.eval::<Vec<i32>>("[1, 2, 3]").unwrap(), vec![1, 2, 3]);
}

#[test]
fn a_huge_sparse_array_passed_to_a_host_function_throws_type_error() {
    let mut js = Context::new();
    js.register_fn("f", |_v: serde_json::Value| 0).unwrap();
    for src in HUGE {
        let t = Instant::now();
        let r = js
            .eval::<String>(&format!(
                "try {{ f({src}); 'no error' }} catch (e) {{ e.name }}"
            ))
            .unwrap();
        assert_eq!(r, "TypeError", "{src}");
        assert!(t.elapsed() < BOUND, "{src}: took {:?}", t.elapsed());
    }
    assert_eq!(js.eval::<i32>("f([1, 2, 3])").unwrap(), 0);
}

#[test]
fn arrays_up_to_the_cap_still_convert() {
    let mut js = Context::new();
    let v = js
        .eval::<Vec<Option<i32>>>(
            "(() => { const a = []; a.length = 100000; a[99999] = 7; return a; })()",
        )
        .unwrap();
    assert_eq!(v.len(), 100_000);
    assert_eq!(v[99_999], Some(7));
    assert_eq!(v[0], None);
}
