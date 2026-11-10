//! The Math object.

use crate::convert::js_pow;
use crate::object::CallArgs;
use crate::realm::Intrinsic;
use crate::value::{Attrs, Value};
use crate::vm::{JsResult, Vm};
use agnijs_engine_api::RealmId;

pub fn install(vm: &mut Vm, realm: RealmId) {
    let math = vm.new_object();
    vm.realm_mut().set_intrinsic(Intrinsic::Math, math);
    vm.constant(math, "E", Value::Number(std::f64::consts::E));
    vm.constant(math, "LN10", Value::Number(std::f64::consts::LN_10));
    vm.constant(math, "LN2", Value::Number(std::f64::consts::LN_2));
    vm.constant(math, "LOG10E", Value::Number(std::f64::consts::LOG10_E));
    vm.constant(math, "LOG2E", Value::Number(std::f64::consts::LOG2_E));
    vm.constant(math, "PI", Value::Number(std::f64::consts::PI));
    vm.constant(
        math,
        "SQRT1_2",
        Value::Number(std::f64::consts::FRAC_1_SQRT_2),
    );
    vm.constant(math, "SQRT2", Value::Number(std::f64::consts::SQRT_2));

    macro_rules! unary {
        ($($name:literal => $f:expr),* $(,)?) => {
            $( {
                fn native(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
                    let x = vm.to_number(a.arg(0))?;
                    let f: fn(f64) -> f64 = $f;
                    Ok(Value::Number(f(x)))
                }
                vm.method(math, $name, 1, native);
            } )*
        };
    }
    unary! {
        "abs" => |x| x.abs(),
        "acos" => |x| x.acos(),
        "acosh" => |x| x.acosh(),
        "asin" => |x| x.asin(),
        "asinh" => |x| if x == 0.0 { x } else { x.asinh() },
        "atan" => |x| x.atan(),
        "atanh" => |x| if x == 0.0 { x } else { x.atanh() },
        "cbrt" => |x| x.cbrt(),
        "ceil" => |x| x.ceil(),
        "cos" => |x| x.cos(),
        "cosh" => |x| x.cosh(),
        "exp" => |x| x.exp(),
        "expm1" => |x| if x == 0.0 { x } else { x.exp_m1() },
        "floor" => |x| x.floor(),
        "fround" => |x| (x as f32) as f64,
        "f16round" => |x| crate::builtins::typedarray::f16_bits_to_f64(crate::builtins::typedarray::f64_to_f16_bits(x)),
        "log" => |x| x.ln(),
        "log1p" => |x| if x == 0.0 { x } else { x.ln_1p() },
        "log10" => |x| x.log10(),
        "log2" => |x| x.log2(),
        "round" => js_round,
        "sign" => |x| if x.is_nan() || x == 0.0 { x } else if x > 0.0 { 1.0 } else { -1.0 },
        "sin" => |x| x.sin(),
        "sinh" => |x| if x == 0.0 { x } else { x.sinh() },
        "sqrt" => |x| x.sqrt(),
        "tan" => |x| x.tan(),
        "tanh" => |x| if x == 0.0 { x } else { x.tanh() },
        "trunc" => |x| x.trunc(),
    }
    vm.method(math, "atan2", 2, math_atan2);
    vm.method(math, "clz32", 1, math_clz32);
    vm.method(math, "hypot", 2, math_hypot);
    vm.method(math, "imul", 2, math_imul);
    vm.method(math, "max", 2, math_max);
    vm.method(math, "min", 2, math_min);
    vm.method(math, "pow", 2, math_pow);
    vm.method(math, "random", 0, math_random);
    super::to_string_tag(vm, math, "Math");
    let global = vm.realm_of(realm).global;
    vm.define_raw_str(global, "Math", Value::Object(math), Attrs::METHOD);
}

fn js_round(x: f64) -> f64 {
    if !x.is_finite() || x.trunc() == x {
        return x;
    }
    if x > 0.0 && x < 0.5 {
        return 0.0;
    }
    if x < 0.0 && x >= -0.5 {
        return -0.0;
    }
    let f = x.floor();
    if x - f >= 0.5 {
        f + 1.0
    } else {
        f
    }
}

fn math_atan2(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let y = vm.to_number(a.arg(0))?;
    let x = vm.to_number(a.arg(1))?;
    Ok(Value::Number(y.atan2(x)))
}

fn math_clz32(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let n = vm.to_uint32(a.arg(0))?;
    Ok(Value::Number(n.leading_zeros() as f64))
}

fn math_hypot(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let mut nums = Vec::with_capacity(a.len());
    for &v in a.args {
        nums.push(vm.to_number(v)?);
    }
    let mut has_nan = false;
    for &n in &nums {
        if n.is_infinite() {
            return Ok(Value::Number(f64::INFINITY));
        }
        if n.is_nan() {
            has_nan = true;
        }
    }
    if has_nan {
        return Ok(Value::Number(f64::NAN));
    }
    if nums.is_empty() {
        return Ok(Value::Number(0.0));
    }
    // Scale to avoid overflow/underflow, then Kahan-summed squares.
    let max = nums.iter().fold(0.0f64, |m, &n| m.max(n.abs()));
    if max == 0.0 {
        return Ok(Value::Number(0.0));
    }
    let mut sum = 0.0;
    let mut comp = 0.0;
    for &n in &nums {
        let r = n / max;
        let y = r * r - comp;
        let t = sum + y;
        comp = (t - sum) - y;
        sum = t;
    }
    Ok(Value::Number(sum.sqrt() * max))
}

fn math_imul(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let x = vm.to_int32(a.arg(0))?;
    let y = vm.to_int32(a.arg(1))?;
    Ok(Value::Number(x.wrapping_mul(y) as f64))
}

fn math_max(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let mut nums = Vec::with_capacity(a.len());
    for &v in a.args {
        nums.push(vm.to_number(v)?);
    }
    let mut r = f64::NEG_INFINITY;
    for n in nums {
        if n.is_nan() {
            return Ok(Value::Number(f64::NAN));
        }
        if n > r || (n == 0.0 && r == 0.0 && !n.is_sign_negative()) {
            r = n;
        }
    }
    Ok(Value::Number(r))
}

fn math_min(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let mut nums = Vec::with_capacity(a.len());
    for &v in a.args {
        nums.push(vm.to_number(v)?);
    }
    let mut r = f64::INFINITY;
    for n in nums {
        if n.is_nan() {
            return Ok(Value::Number(f64::NAN));
        }
        if n < r || (n == 0.0 && r == 0.0 && n.is_sign_negative()) {
            r = n;
        }
    }
    Ok(Value::Number(r))
}

fn math_pow(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let b = vm.to_number(a.arg(0))?;
    let e = vm.to_number(a.arg(1))?;
    Ok(Value::Number(js_pow(b, e)))
}

/// The xorshift64* state for `seed`: splitmix64 spreads nearby seeds
/// apart, and the state is never zero (xorshift's fixed point).
pub(crate) fn rng_state(seed: u64) -> u64 {
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    if z == 0 { 0x9E37_79B9_7F4A_7C15 } else { z }
}

fn math_random(vm: &mut Vm, _a: &CallArgs) -> JsResult<Value> {
    // xorshift64*, one stream per engine, seeded by the host (TASK-103.1).
    let mut s = vm.rng;
    if s == 0 {
        s = rng_state(vm.host.random_seed());
    }
    s ^= s >> 12;
    s ^= s << 25;
    s ^= s >> 27;
    vm.rng = s;
    let r = s.wrapping_mul(0x2545_F491_4F6C_DD1D);
    Ok(Value::Number((r >> 11) as f64 / (1u64 << 53) as f64))
}
