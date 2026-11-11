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
    vm.method(math, "sumPrecise", 1, math_sum_precise);
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

/// Math.sumPrecise(items) (ES2026): the exactly rounded sum of an iterable
/// of Numbers. Every element must already be a Number (no coercion); the
/// first that is not closes the iterator and throws.
fn math_sum_precise(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let items = a.arg(0);
    if !items.is_object() {
        return vm.type_error("Math.sumPrecise requires an iterable object");
    }
    let (it, next) = vm.get_iterator(items, false)?;
    let mut acc = ExactSum::new();
    while let Some(v) = vm.iterator_step_value(it, next)? {
        let Value::Number(x) = v else {
            let f = vm.throw_type("Math.sumPrecise: every element must be a Number");
            vm.iterator_close_swallow(it);
            return Err(f);
        };
        acc.add(x);
    }
    Ok(Value::Number(acc.result()))
}

/// An exact accumulator for sums of doubles: a two's-complement fixed-point
/// integer in units of 2^-1074 (the smallest subnormal), wide enough for
/// any double (2^1024 needs 2098 bits) with 2^62 elements of headroom.
/// Limbs hold 32 bits each in an `i64`, so carries are deferred: each add
/// touches at most three limbs by less than 2^32 each, and the limbs are
/// normalised every 2^30 adds. Overflow, NaN and the sign of zero are
/// tracked beside it as the spec's state machine does.
pub(crate) struct ExactSum {
    limbs: [i64; LIMBS],
    pending: u32,
    pos_inf: bool,
    neg_inf: bool,
    nan: bool,
    /// Only -0 (or nothing) has been added: the result is -0.
    minus_zero: bool,
}

/// 68 limbs of 32 bits: 2176 bits ≥ 2098 + 64 of headroom + sign.
const LIMBS: usize = 68;

impl ExactSum {
    pub(crate) fn new() -> ExactSum {
        ExactSum { limbs: [0; LIMBS], pending: 0, pos_inf: false, neg_inf: false, nan: false, minus_zero: true }
    }

    pub(crate) fn add(&mut self, x: f64) {
        if x.is_nan() {
            self.nan = true;
            return;
        }
        if x.is_infinite() {
            if x > 0.0 { self.pos_inf = true } else { self.neg_inf = true }
            return;
        }
        if x == 0.0 && x.is_sign_negative() {
            return;
        }
        self.minus_zero = false;
        if x == 0.0 {
            return;
        }
        let bits = x.to_bits();
        let biased = ((bits >> 52) & 0x7ff) as u32;
        let frac = bits & ((1u64 << 52) - 1);
        // x = mant * 2^(shift - 1074)
        let (mant, shift) = if biased == 0 { (frac, 0u32) } else { (frac | (1u64 << 52), biased - 1) };
        let limb = (shift / 32) as usize;
        let off = shift % 32;
        let wide = (mant as u128) << off; // < 2^85: three limbs
        let neg = x < 0.0;
        for k in 0..3 {
            let part = ((wide >> (32 * k)) & 0xffff_ffff) as i64;
            if part != 0 {
                if neg { self.limbs[limb + k] -= part } else { self.limbs[limb + k] += part }
            }
        }
        self.pending += 1;
        if self.pending >= 1 << 30 {
            self.normalise();
        }
    }

    /// Carry every limb into 0..2^32, the top one keeping the sign.
    fn normalise(&mut self) {
        let mut carry = 0i64;
        for l in self.limbs.iter_mut().take(LIMBS - 1) {
            let v = *l + carry;
            *l = v & 0xffff_ffff;
            carry = v >> 32;
        }
        self.limbs[LIMBS - 1] += carry;
        self.pending = 0;
    }

    pub(crate) fn result(mut self) -> f64 {
        if self.nan || (self.pos_inf && self.neg_inf) {
            return f64::NAN;
        }
        if self.pos_inf {
            return f64::INFINITY;
        }
        if self.neg_inf {
            return f64::NEG_INFINITY;
        }
        if self.minus_zero {
            return -0.0;
        }
        self.normalise();
        let negative = self.limbs[LIMBS - 1] < 0;
        if negative {
            // Two's-complement negate over 32-bit limbs.
            let mut carry = 1i64;
            for l in self.limbs.iter_mut() {
                let v = (!*l & 0xffff_ffff) + carry;
                *l = v & 0xffff_ffff;
                carry = v >> 32;
            }
        }
        let mag = &self.limbs;
        let Some(top_limb) = (0..LIMBS).rev().find(|&i| mag[i] != 0) else {
            return 0.0;
        };
        let bit = |i: usize| -> u64 { ((mag[i / 32] >> (i % 32)) & 1) as u64 };
        // Index of the highest set bit of the magnitude.
        let top = top_limb * 32 + 63 - (mag[top_limb] as u64).leading_zeros() as usize;
        let value = if top < 53 {
            // Exactly representable as a multiple of 2^-1074.
            let mut m = 0u64;
            for i in (0..=top).rev() {
                m = (m << 1) | bit(i);
            }
            (m as f64) * f64::from_bits(1)
        } else {
            let mut m = 0u64;
            for i in (top - 52..=top).rev() {
                m = (m << 1) | bit(i);
            }
            let round = bit(top - 53) == 1;
            let sticky = top >= 54 && (0..top - 53).any(|i| bit(i) == 1);
            let mut e = top as i64 - 1074; // value in [2^e, 2^(e+1))
            if round && (sticky || m & 1 == 1) {
                m += 1;
                if m == 1u64 << 53 {
                    m >>= 1;
                    e += 1;
                }
            }
            let biased = e + 1023;
            if biased >= 2047 {
                f64::INFINITY
            } else {
                f64::from_bits(((biased as u64) << 52) | (m & ((1u64 << 52) - 1)))
            }
        };
        if negative { -value } else { value }
    }
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
