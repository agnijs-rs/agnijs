//! The named-operand and fused compare-and-branch instructions
//! (TASK-100.18): `BinRR`/`BinRI`/`BinAR`/`BinAI` and `JumpIf[Not]Cmp*`.
//! The compiler emits them for operators on register-held variables and
//! integer literals; these tests pin what must not change when it does:
//! the generic semantics on non-number operands, the spec's evaluation
//! and conversion order, an exception thrown by a conversion mid-instruction
//! leaving the registers as they were, NaN under the negated branches, and
//! the cases where an operand must be copied because the other operand
//! assigns to it.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, RealmId, Value};
use agnijs_js_vm::Vm;

fn engine() -> (Vm, RealmId) {
    let host = TestHost::new();
    let mut vm = Vm::new(host);
    Vm::create_realm(&mut vm);
    let realm = vm.main_realm();
    (vm, realm)
}

/// Evaluates `src` and returns the completion value as a string via
/// `String(...)`, so arrays and primitives compare in one shape.
fn eval_str(src: &str) -> String {
    let (mut vm, realm) = engine();
    let wrapped = format!("String((function () {{ {src} }})())");
    match vm.eval_script(realm, "t.js", &wrapped) {
        Ok(Value::String(s)) => s.to_string(),
        other => panic!("{other:?}: {src}"),
    }
}

// ---- generic semantics on non-number operands -----------------------------------------------

#[test]
fn register_and_immediate_operands_keep_the_operators_full_semantics() {
    // Strings, objects, BigInts, `in` and `instanceof` take the generic
    // path of the same instruction the Number fast path handles.
    assert_eq!(
        eval_str(
            "var a = 'x', b = 'y', n = 3, s = '4', big = 10n, o = { k: 1 }, k = 'k';
             function C() {} var c = new C();
             return [a + b, a + 1, s * 2, s - n, n + s, a < b, s < n, big + 1n, big * big,
                     k in o, c instanceof C, s | 0, s >>> 1, a == 'x', a === b, a != b, n ** 2];"
        ),
        "xy,x1,8,1,34,true,false,11,100,true,true,4,2,true,false,true,9"
    );
}

#[test]
fn immediates_only_stand_on_the_right() {
    // `7 + x` with a string x is concatenation with the number first.
    assert_eq!(eval_str("var x = 'a'; return [7 + x, x + 7, 7 - x, 1000000000 + x];"), "7a,a7,NaN,1000000000a");
}

#[test]
fn big_and_fractional_literals_are_constants_not_immediates() {
    assert_eq!(
        eval_str("var x = 2; return [x * 1.5, x + 1e9, x + -0, 1 / (x * -0), x + 2147483648];"),
        "3,1000000002,2,-Infinity,2147483650"
    );
}

// ---- evaluation and conversion order ----------------------------------------------------------

const LOGGING: &str = "var log = [];
    function mk(n) { return { valueOf() { log.push(n); return n.length; } }; }
    var a = mk('a'), b = mk('bb');";

#[test]
fn relational_operators_convert_the_left_operand_first_in_every_form() {
    // Spec: IsLessThan's LeftFirst flag keeps left-to-right ToPrimitive
    // order for `>` and `<=`, whose operands it swaps.
    let value_forms = format!(
        "{LOGGING} var r = [a < b, a > b, a <= b, a >= b, a == b, a != b];
         return [r.join(), log.join('')];"
    );
    // Equality on two objects compares identity and converts nothing.
    assert_eq!(eval_str(&value_forms), "true,false,true,false,false,true,abbabbabbabb");
    // The same comparisons as branch conditions (fused forms).
    let fused = format!(
        "{LOGGING} var r = [];
         if (a < b) r.push(1); if (a > b) r.push(2); if (a <= b) r.push(3); if (a >= b) r.push(4);
         if (a == b) r.push(5); if (a != b) r.push(6);
         return [r.join(), log.join('')];"
    );
    assert_eq!(eval_str(&fused), "1,3,6,abbabbabbabb");
    // One register operand against a computed right-hand side, and a
    // computed left-hand side against an immediate: the left still goes first.
    let mixed = format!(
        "{LOGGING} var r = [];
         if (a > mk('ccc')) r.push(1); if (a <= mk('ccc')) r.push(2);
         if (mk('dddd') > 3) r.push(3); if (mk('dddd') <= 3) r.push(4);
         if (mk('e') === a) r.push(5);
         return [r.join(), log.join()];"
    );
    assert_eq!(eval_str(&mixed), "2,3,a,ccc,a,ccc,dddd,dddd");
}

#[test]
fn an_operand_the_other_operand_assigns_to_is_read_in_program_order() {
    assert_eq!(
        eval_str(
            "var x = 1, y = 1, i = 1;
             var r = [x + (x = 5), (y = 5) + y];
             r.push(i < (i = 10));
             if (i < (i = 1)) r.push('no'); else r.push(i);
             var p = { a: 'old' }, q = { a: 'new' };
             r.push(p[(p = q, 'a')]);
             var o = { m() { return this.tag; }, tag: 'first' }, other = { tag: 'second' };
             r.push(o.m((o = other, 0)));
             function f(u, v) { return u + ':' + v; }
             var z = 1; r.push(f(z, z = 2));
             var t = { v: 0 }, t2 = { v: 0 }; t.v = (t = t2, 7); r.push(t2.v === 0 && t.v === 0);
             return r;"
        ),
        "6,10,true,1,old,first,1:2,true"
    );
}

// ---- exceptions thrown by a conversion --------------------------------------------------------

#[test]
fn a_conversion_that_throws_mid_instruction_leaves_the_registers_untouched() {
    assert_eq!(
        eval_str(
            "var boom = { valueOf() { throw new Error('boom'); } };
             var a = boom, b = 2, s = 10, r = [];
             try { if (a < b) s = 1; } catch (e) { r.push(e.message, s, b); }
             try { s = a + b; } catch (e) { r.push(s); }
             try { s = a * 2; } catch (e) { r.push(s); }
             try { while (b < a) { s = 2; } } catch (e) { r.push(s, b); }
             try { s = (b + 1) * a; } catch (e) { r.push(s); }
             try { b = b < 3 ? a - 1 : 0; } catch (e) { r.push(b); }
             return r;"
        ),
        "boom,10,2,10,10,10,2,10,2"
    );
}

// ---- NaN and negated branches -----------------------------------------------------------------

#[test]
fn the_negated_branch_forms_are_true_negations_under_nan() {
    assert_eq!(
        eval_str(
            "var n = NaN, z = 0, r = [];
             if (n < z) r.push(1); else r.push(2);
             if (!(n < z)) r.push(3);
             if (n >= z) r.push('no');
             while (!(n >= z)) { r.push(4); break; }
             if (n != n) r.push(5); if (n == n) r.push('no'); if (n !== n) r.push(6);
             if (!(n < 1)) r.push(7); if (!(1 < n)) r.push(8); if (n < 1 || 1 < n) r.push('no');
             do { r.push(9); } while (n < z);
             return r;"
        ),
        "2,3,4,5,6,7,8,9"
    );
}

// ---- conditions ---------------------------------------------------------------------------------

#[test]
fn logical_conditions_branch_directly_with_short_circuit_evaluation() {
    let g = "function g(a, b) {
        var r = [];
        if (a < 1 && b < 1) r.push('both');
        if (a < 1 || b < 1) r.push('either');
        if (!(a < 1) && !(b < 1)) r.push('neither');
        r.push(a < b && b < 3 ? 't' : 'f');
        var i = 0; do { i++; } while (i < 3 && a < 1); r.push(i);
        var k = 0; while (!(k >= 2) || b < 0) k++; r.push(k);
        for (var j = 0; j < 5 && !(j === 3); j++) {} r.push(j);
        return r.join();
    }";
    assert_eq!(
        eval_str(&format!("{g} return [g(0, 0), g(0, 5), g(5, 0), g(5, 5), g(1, 2)];")),
        "both,either,f,3,2,3,either,f,3,2,3,either,f,1,2,3,neither,f,1,2,3,neither,t,1,2,3"
    );
    // The right operand of `&&`/`||` is not evaluated when the left decides.
    assert_eq!(
        eval_str(
            "var log = [];
             function t(v) { log.push(v); return v; }
             if (t(false) && t(true)) {} if (t(true) || t(false)) {} if (!(t(1) < 2) || t(3) < 4) {}
             return log;"
        ),
        "false,true,1,3"
    );
}

#[test]
fn conditions_that_are_not_comparisons_still_work() {
    assert_eq!(
        eval_str(
            "var r = [], o = null, s = '';
             if (o ?? true) r.push(1);
             if (s) r.push('no'); else r.push(2);
             if (!s) r.push(3);
             var n = 0; while (n) {} r.push(4);
             return r;"
        ),
        "1,2,3,4"
    );
}

// ---- updates ------------------------------------------------------------------------------------

#[test]
fn an_update_converts_its_operand_exactly_once_whether_or_not_the_value_is_used() {
    assert_eq!(
        eval_str(
            "var log = [];
             function mk() { return { valueOf() { log.push('v'); return 1; } }; }
             var x = mk(); x++;
             var y = mk(); var old = y++;
             var z = mk(); var now = ++z;
             var w = mk(); for (var i = 0; i < 1; i++, w--) {}
             var o = { p: mk() }; o.p++; var oldp = o.p++;
             return [x, old, y, now, z, w, o.p, oldp, log.join('')];"
        ),
        // Five conversions: the second `o.p++` finds a number.
        "2,1,2,2,2,0,3,2,vvvvv"
    );
    assert_eq!(
        eval_str(
            "var b = 1n; b++; var c = 1n; var o = c++; var d = 1n; --d;
             return [b, c, o, d, typeof b, typeof o];"
        ),
        "2,2,1,0,bigint,bigint"
    );
}
