//! One instruction at a time: the big match.

use super::{Flow, RunOutcome};
use crate::convert::Hint;
use crate::object::{Class, FieldKey, ObjectData};
use crate::symbol::{PrivateElement, SymbolData, SymbolKind};
use crate::value::{Attrs, PropKey, PropSlot, Property, PropertyDescriptor, Value};
use crate::vm::{ErrorType, Fault, JsResult, Vm};
use gable_js_compiler::bytecode::{
    CodeBlock, Constant, ErrorKind, Instr, NamePrefix, PrivateKind, RESUME_RETURN, RESUME_THROW,
};
use std::rc::Rc;

impl Vm {
    #[inline]
    pub(crate) fn constant_value(&mut self, code: &CodeBlock, idx: u32) -> Value {
        match &code.constants[idx as usize] {
            Constant::Number(n) => Value::Number(*n),
            Constant::String(s) => Value::String(self.heap.alloc_str_utf16(s.to_vec())),
            Constant::BigInt(text) => {
                let b = crate::bigint::parse_literal(text);
                Value::BigInt(self.heap.alloc_bigint(crate::bigint::BigIntData(b)))
            }
            Constant::Template(_) => panic!("template constant loaded as a value"),
        }
    }

    /// The message string of a constant.
    pub(crate) fn constant_message(&mut self, code: &CodeBlock, idx: u32) -> String {
        match &code.constants[idx as usize] {
            Constant::String(s) => String::from_utf16_lossy(s),
            Constant::Number(n) => n.to_string(),
            _ => String::new(),
        }
    }

    #[inline]
    pub(crate) fn name_key(&mut self, code: &CodeBlock, idx: u32) -> PropKey {
        let name: &str = &code.names[idx as usize];
        self.key(name)
    }

    #[inline]
    pub(crate) fn name_str(&mut self, code: &CodeBlock, idx: u32) -> Rc<str> {
        code.names[idx as usize].clone()
    }

    pub(crate) fn step(
        &mut self,
        instr: Instr,
        fi: usize,
        base: usize,
        code: &Rc<CodeBlock>,
    ) -> JsResult<Flow> {
        macro_rules! reg {
            ($r:expr) => {
                self.regs[base + ($r) as usize]
            };
        }
        macro_rules! acc {
            () => {
                self.frames[fi].acc
            };
        }
        macro_rules! set_acc {
            ($v:expr) => {{
                let v = $v;
                self.frames[fi].acc = v;
            }};
        }
        macro_rules! jump {
            ($off:expr) => {{
                let pc = self.frames[fi].pc - 1;
                let off = $off;
                if off <= 0 {
                    self.poll_interrupt()?;
                }
                self.frames[fi].pc = (pc as i64 + off as i64) as usize;
            }};
        }
        macro_rules! args_vec {
            ($args:expr, $argc:expr) => {{
                let start = base + ($args) as usize;
                self.regs[start..start + ($argc) as usize].to_vec()
            }};
        }

        match instr {
            // ---- loads and moves ----
            Instr::LdaUndefined => set_acc!(Value::Undefined),
            Instr::LdaNull => set_acc!(Value::Null),
            Instr::LdaTrue => set_acc!(Value::Bool(true)),
            Instr::LdaFalse => set_acc!(Value::Bool(false)),
            Instr::LdaEmpty => set_acc!(Value::Empty),
            Instr::LdaInt(i) => set_acc!(Value::Number(i as f64)),
            Instr::LdaConst(c) => {
                let v = self.constant_value(code, c);
                set_acc!(v);
            }
            Instr::Ldar(r) => set_acc!(reg!(r)),
            Instr::Star(r) => {
                let v = acc!();
                reg!(r) = v;
            }
            Instr::Mov { dst, src } => {
                let v = reg!(src);
                reg!(dst) = v;
            }

            // ---- environments ----
            Instr::PushEnv(scope) => {
                let parent = self.frames[fi].env;
                let env = self.heap.alloc_env(crate::env::Environment::declarative(
                    Some((code.clone(), scope)),
                    parent,
                ));
                let f = &mut self.frames[fi];
                f.env = Some(env);
                f.env_depth += 1;
            }
            Instr::PopEnv => {
                let f = &mut self.frames[fi];
                let e = f.env.expect("PopEnv with no environment");
                f.env = self.heap.env(e).parent();
                f.env_depth -= 1;
            }
            Instr::CopyEnv => {
                let e = self.frames[fi].env.expect("CopyEnv with no environment");
                let copy = self.heap.env(e).copy_declarative();
                let n = self.heap.alloc_env(copy);
                self.frames[fi].env = Some(n);
            }
            Instr::PushWith => {
                let v = acc!();
                let obj = self.to_object(v)?;
                let parent = self.frames[fi].env;
                let env = self
                    .heap
                    .alloc_env(crate::env::Environment::with_object(obj, parent));
                let f = &mut self.frames[fi];
                f.env = Some(env);
                f.env_depth += 1;
            }
            Instr::LdaEnv { depth, slot } => {
                let e = self.env_at(fi, depth);
                let v = match self.heap.env(e).as_decl() {
                    Some(d) => d.slots[slot as usize],
                    None => panic!(
                        "LdaEnv on a non-declarative environment in {} ({:?}) at pc {} (depth {depth}, slot {slot})",
                        code.name, code.kind, self.frames[fi].pc - 1
                    ),
                };
                set_acc!(v);
            }
            Instr::StaEnv { depth, slot } => {
                let e = self.env_at(fi, depth);
                let v = acc!();
                self.heap
                    .env_mut(e)
                    .as_decl_mut()
                    .expect("StaEnv on a non-declarative environment")
                    .slots[slot as usize] = v;
            }
            Instr::CheckTdz(name) => {
                if acc!().is_empty() {
                    let n = self.name_str(code, name);
                    let msg = format!("Cannot access '{}' before initialization", display_name(&n));
                    return Err(self.throw_reference(&msg));
                }
            }
            Instr::ThrowConstAssign(name) => {
                let n = self.name_str(code, name);
                let msg = format!("Assignment to constant variable '{}'", display_name(&n));
                return Err(self.throw_type(&msg));
            }
            Instr::LdaGlobal(name) => {
                let v = self.load_global(code, name, false)?;
                set_acc!(v);
            }
            Instr::LdaGlobalOrUndefined(name) => {
                let v = self.load_global(code, name, true)?;
                set_acc!(v);
            }
            Instr::StaGlobal { name, strict } => {
                let v = acc!();
                self.store_global(code, name, v, strict)?;
            }
            Instr::InitGlobalLexical(name) => {
                let v = acc!();
                let n = self.name_str(code, name);
                let realm = self.frames[fi].realm;
                if let Some(b) = self.realms[realm.0 as usize].global_lexicals.get_mut(&*n) {
                    b.value = v;
                }
            }
            Instr::LdaDynamic(name) => {
                let v = self.load_dynamic(fi, code, name, false)?;
                set_acc!(v);
            }
            Instr::LdaDynamicOrUndefined(name) => {
                let v = self.load_dynamic(fi, code, name, true)?;
                set_acc!(v);
            }
            Instr::StaDynamic { name, strict } => {
                let v = acc!();
                self.store_dynamic(fi, code, name, v, strict, false)?;
            }
            Instr::StaEvalVar(name) => {
                let v = acc!();
                self.store_eval_var(fi, code, name, v)?;
            }
            Instr::InitDynamic(name) => {
                let v = acc!();
                self.store_dynamic(fi, code, name, v, true, true)?;
            }
            Instr::DeleteDynamic(name) => {
                let r = self.delete_dynamic(fi, code, name)?;
                set_acc!(Value::Bool(r));
            }
            Instr::ResolveDynamic { name, dst } => {
                let r = self.resolve_dynamic_ref(fi, code, name)?;
                reg!(dst) = r;
            }
            Instr::LdaDynamicRef { name, reference } => {
                let r = reg!(reference);
                let v = self.load_dynamic_ref(fi, code, name, r)?;
                set_acc!(v);
            }
            Instr::StaDynamicRef {
                name,
                reference,
                strict,
            } => {
                let r = reg!(reference);
                let v = acc!();
                self.store_dynamic_ref(fi, code, name, r, v, strict)?;
            }
            Instr::LdaRefThis(reference) => {
                let r = reg!(reference);
                set_acc!(match r {
                    Value::Object(_) => r,
                    _ => Value::Undefined,
                });
            }
            Instr::ThisForDynamicCall(name) => {
                let v = self.this_for_dynamic_call(fi, code, name)?;
                set_acc!(v);
            }
            Instr::DeclareGlobals => {
                self.declare_globals(fi, code)?;
            }
            Instr::LdaImport(idx) => {
                let v = crate::modules::load_import(self, fi, idx)?;
                set_acc!(v);
            }

            // ---- property access ----
            Instr::GetNamed { obj, name } => {
                let o = reg!(obj);
                let key = self.name_key(code, name);
                let v = self.get_value_named(o, key, &code.names[name as usize].clone())?;
                set_acc!(v);
            }
            Instr::SetNamed { obj, name, strict } => {
                let o = reg!(obj);
                let key = self.name_key(code, name);
                let v = acc!();
                self.put_value(o, key, v, strict)?;
            }
            Instr::GetKeyed { obj, key } => {
                let o = reg!(obj);
                let k = reg!(key);
                if o.is_nullish() {
                    let ks = self.display_key_value(k)?;
                    let msg = format!(
                        "Cannot read properties of {} (reading '{}')",
                        if o.is_null() { "null" } else { "undefined" },
                        ks
                    );
                    return Err(self.throw_type(&msg));
                }
                let pk = self.to_property_key(k)?;
                let v = self.get_value(o, pk)?;
                set_acc!(v);
            }
            Instr::SetKeyed { obj, key, strict } => {
                let o = reg!(obj);
                let k = reg!(key);
                if o.is_nullish() {
                    let ks = self.display_key_value(k)?;
                    let msg = format!(
                        "Cannot set properties of {} (setting '{}')",
                        if o.is_null() { "null" } else { "undefined" },
                        ks
                    );
                    return Err(self.throw_type(&msg));
                }
                let pk = self.to_property_key(k)?;
                let v = acc!();
                self.put_value(o, pk, v, strict)?;
            }
            Instr::DeleteNamed { obj, name, strict } => {
                let o = reg!(obj);
                let key = self.name_key(code, name);
                let r = self.delete_value(o, key, strict)?;
                set_acc!(Value::Bool(r));
            }
            Instr::DeleteKeyed { obj, key, strict } => {
                let o = reg!(obj);
                let k = reg!(key);
                if o.is_nullish() {
                    return Err(self.throw_type("Cannot convert undefined or null to object"));
                }
                let pk = self.to_property_key(k)?;
                let r = self.delete_value(o, pk, strict)?;
                set_acc!(Value::Bool(r));
            }
            Instr::GetSuper { this, func, key } => {
                let t = reg!(this);
                let f = reg!(func);
                let k = reg!(key);
                let home = self.home_object_of(f)?;
                let base = self.get_prototype_of(home)?;
                let pk = self.to_property_key(k)?;
                let v = match base {
                    Some(p) => self.get_prop(p, pk, t)?,
                    None => {
                        return Err(self.throw_type("Cannot read properties of null (super base)"))
                    }
                };
                set_acc!(v);
            }
            Instr::SetSuper {
                this,
                func,
                key,
                strict,
            } => {
                let t = reg!(this);
                let f = reg!(func);
                let k = reg!(key);
                let v = acc!();
                let home = self.home_object_of(f)?;
                let base = self.get_prototype_of(home)?;
                let pk = self.to_property_key(k)?;
                let ok = match base {
                    Some(p) => self.set_prop(p, pk, v, t)?,
                    None => {
                        return Err(self.throw_type("Cannot set properties of null (super base)"))
                    }
                };
                if !ok && strict {
                    let ks = self.display_key(pk);
                    return Err(
                        self.throw_type(&format!("Cannot assign to read only property '{ks}'"))
                    );
                }
            }
            Instr::DefineOwn { obj, key } => {
                let o = reg!(obj).as_object().expect("DefineOwn on a non-object");
                let k = reg!(key);
                let pk = self.to_property_key(k)?;
                let v = acc!();
                self.create_data_property_or_throw(o, pk, v)?;
            }
            Instr::DefineOwnNamed { obj, name } => {
                let o = reg!(obj)
                    .as_object()
                    .expect("DefineOwnNamed on a non-object");
                let pk = self.name_key(code, name);
                let v = acc!();
                self.create_data_property_or_throw(o, pk, v)?;
            }
            Instr::DefineMethod {
                obj,
                key,
                enumerable,
            } => {
                let o = reg!(obj).as_object().expect("DefineMethod on a non-object");
                let k = reg!(key);
                let f = acc!()
                    .as_object()
                    .expect("DefineMethod with a non-function");
                let pk = self.to_property_key(k)?;
                let kv = self.key_to_value(pk);
                self.set_function_name(f, kv, None);
                if let Some(fd) = self.heap.obj_mut(f).function_mut() {
                    fd.home_object = Some(o);
                }
                let desc = PropertyDescriptor {
                    value: Some(Value::Object(f)),
                    writable: Some(true),
                    enumerable: Some(enumerable),
                    configurable: Some(true),
                    ..Default::default()
                };
                self.define_property_or_throw(o, pk, desc)?;
            }
            Instr::DefineGetter {
                obj,
                key,
                enumerable,
            }
            | Instr::DefineSetter {
                obj,
                key,
                enumerable,
            } => {
                let is_getter = matches!(instr, Instr::DefineGetter { .. });
                let o = reg!(obj).as_object().expect("DefineGetter on a non-object");
                let k = reg!(key);
                let f = acc!()
                    .as_object()
                    .expect("DefineGetter with a non-function");
                let pk = self.to_property_key(k)?;
                let kv = self.key_to_value(pk);
                self.set_function_name(f, kv, Some(if is_getter { "get" } else { "set" }));
                if let Some(fd) = self.heap.obj_mut(f).function_mut() {
                    fd.home_object = Some(o);
                }
                let desc = PropertyDescriptor {
                    get: if is_getter {
                        Some(Value::Object(f))
                    } else {
                        None
                    },
                    set: if is_getter {
                        None
                    } else {
                        Some(Value::Object(f))
                    },
                    enumerable: Some(enumerable),
                    configurable: Some(true),
                    ..Default::default()
                };
                self.define_property_or_throw(o, pk, desc)?;
            }
            Instr::SetHomeObject { func, home } => {
                let f = reg!(func)
                    .as_object()
                    .expect("SetHomeObject on a non-function");
                let h = reg!(home)
                    .as_object()
                    .expect("SetHomeObject with a non-object home");
                if let Some(fd) = self.heap.obj_mut(f).function_mut() {
                    fd.home_object = Some(h);
                }
            }
            Instr::SetFunctionName { key, prefix } => {
                let k = reg!(key);
                if let Value::Object(f) = acc!() {
                    let kv = match k {
                        Value::Symbol(_) | Value::String(_) => k,
                        other => {
                            let pk = self.to_property_key(other)?;
                            self.key_to_value(pk)
                        }
                    };
                    let p = match prefix {
                        NamePrefix::None => None,
                        NamePrefix::Get => Some("get"),
                        NamePrefix::Set => Some("set"),
                    };
                    self.set_function_name(f, kv, p);
                }
            }
            Instr::SetPrototypeLiteral { obj } => {
                let o = reg!(obj)
                    .as_object()
                    .expect("SetPrototypeLiteral on a non-object");
                match acc!() {
                    Value::Object(p) => {
                        self.heap.obj_mut(o).proto = Some(p);
                    }
                    Value::Null => {
                        self.heap.obj_mut(o).proto = None;
                    }
                    _ => {}
                }
            }
            Instr::CopyDataProperties {
                target,
                excluded,
                count,
            } => {
                let t = reg!(target)
                    .as_object()
                    .expect("CopyDataProperties on a non-object");
                let src = acc!();
                let mut ex = Vec::with_capacity(count as usize);
                for i in 0..count {
                    let kv = reg!(excluded + i);
                    ex.push(self.to_property_key(kv)?);
                }
                self.copy_data_properties(t, src, &ex)?;
            }

            // ---- private names ----
            Instr::NewPrivateName(name) => {
                let n = self.name_str(code, name);
                let desc = self.heap.alloc_str(&n);
                let sym = self
                    .heap
                    .alloc_symbol(SymbolData::new(Some(desc), SymbolKind::Private));
                set_acc!(Value::Symbol(sym));
            }
            Instr::GetPrivate { obj, name } => {
                let o = reg!(obj);
                let n = reg!(name).as_symbol_id();
                let v = self.private_get(o, n)?;
                set_acc!(v);
            }
            Instr::SetPrivate { obj, name } => {
                let o = reg!(obj);
                let n = reg!(name).as_symbol_id();
                let v = acc!();
                self.private_set(o, n, v)?;
            }
            Instr::HasPrivate { obj, name } => {
                let o = reg!(obj);
                let n = reg!(name).as_symbol_id();
                let Value::Object(o) = o else {
                    return Err(self.throw_type(
                        "Cannot use 'in' operator to search for a private field in a non-object",
                    ));
                };
                let has = self.heap.has_own_property(o, PropKey::Sym(n));
                set_acc!(Value::Bool(has));
            }
            Instr::SetPrivateMethod { name, kind } => {
                let n = reg!(name).as_symbol_id();
                let f = acc!();
                let desc = self
                    .heap
                    .sym(n)
                    .description
                    .map(Value::String)
                    .unwrap_or(Value::Undefined);
                if let Value::Object(fo) = f {
                    let prefix = match kind {
                        PrivateKind::Method => None,
                        PrivateKind::Getter => Some("get"),
                        PrivateKind::Setter => Some("set"),
                    };
                    self.set_function_name(fo, desc, prefix);
                }
                let s = self.heap.sym_mut(n);
                s.private = match (kind, s.private) {
                    (PrivateKind::Method, _) => PrivateElement::Method(f),
                    (PrivateKind::Getter, PrivateElement::Accessor { set, .. }) => {
                        PrivateElement::Accessor { get: f, set }
                    }
                    (PrivateKind::Getter, _) => PrivateElement::Accessor {
                        get: f,
                        set: Value::Undefined,
                    },
                    (PrivateKind::Setter, PrivateElement::Accessor { get, .. }) => {
                        PrivateElement::Accessor { get, set: f }
                    }
                    (PrivateKind::Setter, _) => PrivateElement::Accessor {
                        get: Value::Undefined,
                        set: f,
                    },
                };
            }
            Instr::AddPrivateBrand { obj, name } => {
                let o = reg!(obj)
                    .as_object()
                    .expect("AddPrivateBrand on a non-object");
                let n = reg!(name).as_symbol_id();
                self.private_method_add(o, n)?;
            }

            // ---- operators ----
            Instr::Add(r) => {
                let l = reg!(r);
                let v = self.op_add(l, acc!())?;
                set_acc!(v);
            }
            Instr::Sub(r) | Instr::Mul(r) | Instr::Div(r) | Instr::Mod(r) | Instr::Exp(r) => {
                let l = reg!(r);
                let v = self.op_arith(instr_arith(instr), l, acc!())?;
                set_acc!(v);
            }
            Instr::Shl(r)
            | Instr::Shr(r)
            | Instr::UShr(r)
            | Instr::BitAnd(r)
            | Instr::BitOr(r)
            | Instr::BitXor(r) => {
                let l = reg!(r);
                let v = self.op_bitwise(instr_arith(instr), l, acc!())?;
                set_acc!(v);
            }
            Instr::Eq(r) => {
                let l = reg!(r);
                let v = self.loose_equals(l, acc!())?;
                set_acc!(Value::Bool(v));
            }
            Instr::Ne(r) => {
                let l = reg!(r);
                let v = self.loose_equals(l, acc!())?;
                set_acc!(Value::Bool(!v));
            }
            Instr::StrictEq(r) => {
                let l = reg!(r);
                let v = self.strict_equals(l, acc!());
                set_acc!(Value::Bool(v));
            }
            Instr::StrictNe(r) => {
                let l = reg!(r);
                let v = self.strict_equals(l, acc!());
                set_acc!(Value::Bool(!v));
            }
            Instr::Lt(r) => {
                let l = reg!(r);
                let v = self.op_less_than(l, acc!(), true)?;
                set_acc!(Value::Bool(v == Some(true)));
            }
            Instr::Gt(r) => {
                let l = reg!(r);
                let v = self.op_less_than(acc!(), l, false)?;
                set_acc!(Value::Bool(v == Some(true)));
            }
            Instr::Le(r) => {
                let l = reg!(r);
                let v = self.op_less_than(acc!(), l, false)?;
                set_acc!(Value::Bool(v == Some(false)));
            }
            Instr::Ge(r) => {
                let l = reg!(r);
                let v = self.op_less_than(l, acc!(), true)?;
                set_acc!(Value::Bool(v == Some(false)));
            }
            Instr::InstanceOf(r) => {
                let l = reg!(r);
                let v = self.instance_of(l, acc!())?;
                set_acc!(Value::Bool(v));
            }
            Instr::In(r) => {
                let l = reg!(r);
                let Value::Object(o) = acc!() else {
                    return Err(self.throw_type(
                        "Cannot use 'in' operator to search for a key in a non-object",
                    ));
                };
                let pk = self.to_property_key(l)?;
                let v = self.has_property(o, pk)?;
                set_acc!(Value::Bool(v));
            }
            Instr::Neg => {
                let v = self.to_numeric(acc!())?;
                let r = match v {
                    Value::Number(n) => Value::Number(-n),
                    Value::BigInt(b) => {
                        let n = -self.heap.bigint(b).0.clone();
                        self.bigint_value(n)
                    }
                    _ => unreachable!(),
                };
                set_acc!(r);
            }
            Instr::Pos => {
                let n = self.to_number(acc!())?;
                set_acc!(Value::Number(n));
            }
            Instr::Not => {
                let b = self.to_boolean(acc!());
                set_acc!(Value::Bool(!b));
            }
            Instr::BitNot => {
                let v = self.to_numeric(acc!())?;
                let r = match v {
                    Value::Number(n) => Value::Number(!crate::convert::f64_to_int32(n) as f64),
                    Value::BigInt(b) => {
                        let n = !self.heap.bigint(b).0.clone();
                        self.bigint_value(n)
                    }
                    _ => unreachable!(),
                };
                set_acc!(r);
            }
            Instr::Typeof => {
                let s = self.typeof_value(acc!());
                set_acc!(Value::String(s));
            }
            Instr::ToNumeric => {
                let v = self.to_numeric(acc!())?;
                set_acc!(v);
            }
            Instr::Inc | Instr::Dec => {
                let delta = if matches!(instr, Instr::Inc) {
                    1.0
                } else {
                    -1.0
                };
                let v = self.to_numeric(acc!())?;
                let r = match v {
                    Value::Number(n) => Value::Number(n + delta),
                    Value::BigInt(b) => {
                        let n =
                            self.heap.bigint(b).0.clone() + num_bigint::BigInt::from(delta as i64);
                        self.bigint_value(n)
                    }
                    _ => unreachable!(),
                };
                set_acc!(r);
            }
            Instr::ToString => {
                let s = self.to_string(acc!())?;
                set_acc!(Value::String(s));
            }
            Instr::ToPropertyKey => {
                let pk = self.to_property_key(acc!())?;
                let v = self.key_to_value(pk);
                set_acc!(v);
            }
            Instr::RequireObjectCoercible => {
                let v = acc!();
                if v.is_nullish() {
                    let msg = format!(
                        "Cannot destructure '{}' as it is {}.",
                        if v.is_null() { "null" } else { "undefined" },
                        if v.is_null() { "null" } else { "undefined" }
                    );
                    return Err(self.throw_type(&msg));
                }
            }

            // ---- control flow ----
            Instr::Jump(off) => jump!(off),
            Instr::JumpIfTrue(off) => {
                if self.to_boolean(acc!()) {
                    jump!(off);
                }
            }
            Instr::JumpIfFalse(off) => {
                if !self.to_boolean(acc!()) {
                    jump!(off);
                }
            }
            Instr::JumpIfNullish(off) => {
                if acc!().is_nullish() {
                    jump!(off);
                }
            }
            Instr::JumpIfNotNullish(off) => {
                if !acc!().is_nullish() {
                    jump!(off);
                }
            }
            Instr::JumpIfUndefined(off) => {
                if acc!().is_undefined() {
                    jump!(off);
                }
            }
            Instr::JumpIfNotUndefined(off) => {
                if !acc!().is_undefined() {
                    jump!(off);
                }
            }
            Instr::JumpIfEmpty(off) => {
                if acc!().is_empty() {
                    jump!(off);
                }
            }
            Instr::JumpIfNotEmpty(off) => {
                if !acc!().is_empty() {
                    jump!(off);
                }
            }
            Instr::JumpIfObject(off) => {
                if acc!().is_object() {
                    jump!(off);
                }
            }
            Instr::JumpIfStrictEq { reg, offset } => {
                let l = reg!(reg);
                if self.strict_equals(l, acc!()) {
                    jump!(offset);
                }
            }

            // ---- calls ----
            Instr::Call {
                callee,
                this,
                args,
                argc,
            } => {
                let f = reg!(callee);
                let t = reg!(this);
                let a = args_vec!(args, argc);
                return self.call_from_frame(f, t, a);
            }
            Instr::CallSpread { callee, this, args } => {
                let f = reg!(callee);
                let t = reg!(this);
                let arr = reg!(args).as_object().expect("CallSpread with a non-array");
                let a = self.array_to_vec(arr);
                return self.call_from_frame(f, t, a);
            }
            Instr::CallEval {
                callee,
                this,
                args,
                argc,
                strict,
            } => {
                let f = reg!(callee);
                let t = reg!(this);
                let a = args_vec!(args, argc);
                if let Value::Object(fo) = f {
                    if fo == self.intrinsic_in(self.frames[fi].realm, crate::realm::Intrinsic::Eval)
                    {
                        let v = self.direct_eval(
                            fi,
                            a.first().copied().unwrap_or(Value::Undefined),
                            strict,
                        )?;
                        set_acc!(v);
                        return Ok(Flow::Next);
                    }
                }
                return self.call_from_frame(f, t, a);
            }
            Instr::CallEvalSpread {
                callee,
                this,
                args,
                strict,
            } => {
                let f = reg!(callee);
                let t = reg!(this);
                let arr = reg!(args)
                    .as_object()
                    .expect("CallEvalSpread with a non-array");
                let a = self.array_to_vec(arr);
                if let Value::Object(fo) = f {
                    if fo == self.intrinsic_in(self.frames[fi].realm, crate::realm::Intrinsic::Eval)
                    {
                        let v = self.direct_eval(
                            fi,
                            a.first().copied().unwrap_or(Value::Undefined),
                            strict,
                        )?;
                        set_acc!(v);
                        return Ok(Flow::Next);
                    }
                }
                return self.call_from_frame(f, t, a);
            }
            Instr::Construct { callee, args, argc } => {
                let f = reg!(callee);
                let a = args_vec!(args, argc);
                return self.construct_from_frame(f, a, f);
            }
            Instr::ConstructSpread { callee, args } => {
                let f = reg!(callee);
                let arr = reg!(args)
                    .as_object()
                    .expect("ConstructSpread with a non-array");
                let a = self.array_to_vec(arr);
                return self.construct_from_frame(f, a, f);
            }
            Instr::SuperCall {
                func,
                new_target,
                args,
                argc,
            } => {
                let f = reg!(func)
                    .as_object()
                    .expect("SuperCall with a non-function");
                let nt = reg!(new_target);
                let a = args_vec!(args, argc);
                let parent = self.get_prototype_of(f)?;
                let parent = match parent {
                    Some(p) if self.is_constructor(Value::Object(p)) => Value::Object(p),
                    _ => return Err(self.throw_type("Super constructor is not a constructor")),
                };
                return self.construct_from_frame(parent, a, nt);
            }
            Instr::SuperCallSpread {
                func,
                new_target,
                args,
            } => {
                let f = reg!(func)
                    .as_object()
                    .expect("SuperCallSpread with a non-function");
                let nt = reg!(new_target);
                let arr = reg!(args)
                    .as_object()
                    .expect("SuperCallSpread with a non-array");
                let a = self.array_to_vec(arr);
                let parent = self.get_prototype_of(f)?;
                let parent = match parent {
                    Some(p) if self.is_constructor(Value::Object(p)) => Value::Object(p),
                    _ => return Err(self.throw_type("Super constructor is not a constructor")),
                };
                return self.construct_from_frame(parent, a, nt);
            }
            Instr::Return => {
                return Ok(self.do_return());
            }
            Instr::CheckDerivedReturn { this } => {
                let v = acc!();
                if !v.is_object() {
                    // [[Construct]] performs these checks after the body has
                    // completed, so the frame's own handlers must not see
                    // the error.
                    if !v.is_undefined() {
                        self.uncatchable = true;
                        return Err(self.throw_type(
                            "Derived constructors may only return object or undefined",
                        ));
                    }
                    let t = reg!(this);
                    if t.is_empty() {
                        self.uncatchable = true;
                        return Err(self.throw_reference("Must call super constructor in derived class before accessing 'this' or returning from derived constructor"));
                    }
                    set_acc!(t);
                }
            }
            Instr::Throw => {
                return Err(Fault::Throw(acc!()));
            }
            Instr::ThrowError { kind, message } => {
                let msg = self.constant_message(code, message);
                let ty = match kind {
                    ErrorKind::Type => ErrorType::TypeError,
                    ErrorKind::Reference => ErrorType::ReferenceError,
                    ErrorKind::Syntax => ErrorType::SyntaxError,
                    ErrorKind::Range => ErrorType::RangeError,
                };
                return Err(self.throw(ty, &msg));
            }
            Instr::ThrowIfNotObject(message) => {
                if !acc!().is_object() {
                    let msg = self.constant_message(code, message);
                    return Err(self.throw_type(&msg));
                }
            }

            // ---- literals and functions ----
            Instr::CreateObject => {
                let o = self.new_object();
                set_acc!(Value::Object(o));
            }
            Instr::CreateArray => {
                let a = self.new_array(Vec::new());
                set_acc!(Value::Object(a));
            }
            Instr::ArrayPush { arr } => {
                let a = reg!(arr).as_object().expect("ArrayPush on a non-array");
                let v = acc!();
                if !self.heap.array_push_dense(a, v) {
                    let len = self.heap.obj(a).array_length().unwrap_or(0);
                    self.create_data_property_or_throw(a, PropKey::Index(len), v)?;
                }
            }
            Instr::ArrayPushHole { arr } => {
                let a = reg!(arr).as_object().expect("ArrayPushHole on a non-array");
                if let Class::Array(ad) = &mut self.heap.obj_mut(a).class {
                    ad.length += 1;
                }
            }
            Instr::ArraySpread { arr } => {
                let a = reg!(arr).as_object().expect("ArraySpread on a non-array");
                let src = acc!();
                let (iter, next) = self.get_iterator(src, false)?;
                loop {
                    match self.iterator_step_value(iter, next)? {
                        Some(v) => {
                            if !self.heap.array_push_dense(a, v) {
                                let len = self.heap.obj(a).array_length().unwrap_or(0);
                                self.create_data_property_or_throw(a, PropKey::Index(len), v)?;
                            }
                        }
                        None => break,
                    }
                }
            }
            Instr::CreateClosure(idx) => {
                let fcode = code.functions[idx as usize].clone();
                let env = self.frames[fi].env;
                let module = self.frames[fi].module;
                let f = self.create_function(fcode, env, module);
                set_acc!(Value::Object(f));
            }
            Instr::CreateClass {
                func,
                heritage,
                proto,
                name,
            } => {
                let fcode = code.functions[func as usize].clone();
                let h = reg!(heritage);
                let n = reg!(name);
                let (ctor, prototype) = self.create_class(fcode, h, n, fi)?;
                reg!(proto) = Value::Object(prototype);
                set_acc!(Value::Object(ctor));
            }
            Instr::AddField {
                ctor,
                key,
                init,
                anon,
            } => {
                let c = reg!(ctor).as_object().expect("AddField on a non-class");
                let k = reg!(key);
                let i = reg!(init);
                let fk = match k {
                    Value::Symbol(s) if self.heap.sym(s).is_private() => FieldKey::Private(s),
                    other => {
                        let pk = self.to_property_key(other)?;
                        FieldKey::Prop(pk)
                    }
                };
                let def = crate::object::FieldDef {
                    key: fk,
                    init: i.as_object(),
                    anon,
                };
                if let Some(fd) = self.heap.obj_mut(c).function_mut() {
                    fd.class_data
                        .get_or_insert_with(Default::default)
                        .fields
                        .push(def);
                }
            }
            Instr::DefineField { obj, key, anon } => {
                let o = reg!(obj).as_object().expect("DefineField on a non-object");
                let k = reg!(key);
                let v = acc!();
                let fk = match k {
                    Value::Symbol(s) if self.heap.sym(s).is_private() => FieldKey::Private(s),
                    other => {
                        let pk = self.to_property_key(other)?;
                        FieldKey::Prop(pk)
                    }
                };
                self.define_field(o, &fk, v, anon)?;
            }
            Instr::AddInstancePrivateMethod { ctor, name } => {
                let c = reg!(ctor)
                    .as_object()
                    .expect("AddInstancePrivateMethod on a non-class");
                let n = reg!(name).as_symbol_id();
                if let Some(fd) = self.heap.obj_mut(c).function_mut() {
                    fd.class_data
                        .get_or_insert_with(Default::default)
                        .private_methods
                        .push(n);
                }
            }
            Instr::InitializeInstanceElements { func, this } => {
                let f = reg!(func)
                    .as_object()
                    .expect("InitializeInstanceElements with a non-function");
                let t = reg!(this)
                    .as_object()
                    .expect("InitializeInstanceElements with a non-object this");
                self.initialize_instance_elements(f, t)?;
            }
            Instr::CreateRegExp { pattern, flags } => {
                let p = self.constant_value(code, pattern).as_string().unwrap();
                let f = self.constant_value(code, flags).as_string().unwrap();
                let r = crate::builtins::regexp::regexp_create(self, p, f)?;
                set_acc!(r);
            }
            Instr::GetTemplateObject(c) => {
                let t = self.get_template_object(code, c);
                set_acc!(Value::Object(t));
            }
            Instr::CreateArguments { mapped } => {
                let a = self.create_arguments_object(fi, code, mapped);
                set_acc!(Value::Object(a));
            }
            Instr::CreateRestArray { from } => {
                let items: Vec<Value> = self.frames[fi]
                    .args
                    .iter()
                    .skip(from as usize)
                    .copied()
                    .collect();
                let a = self.new_array(items);
                set_acc!(Value::Object(a));
            }
            Instr::LdaCallee => {
                let c = self.frames[fi]
                    .callee
                    .map(Value::Object)
                    .unwrap_or(Value::Undefined);
                set_acc!(c);
            }
            Instr::LdaNewTarget => {
                let nt = self.frames[fi].new_target;
                set_acc!(nt);
            }
            Instr::LdaThis => {
                let t = self.frames[fi].this;
                set_acc!(t);
            }
            Instr::ImportMeta => {
                let v = crate::modules::import_meta(self, fi)?;
                set_acc!(v);
            }
            Instr::DynamicImport { spec, options } => {
                let s = reg!(spec);
                let o = reg!(options);
                let v = crate::modules::dynamic_import_deferred(self, fi, s, o)?;
                set_acc!(v);
            }

            // ---- iteration ----
            Instr::GetIterator { iter, next } => {
                let v = acc!();
                let (i, n) = self.get_iterator(v, false)?;
                reg!(iter) = i;
                reg!(next) = n;
            }
            Instr::GetAsyncIterator { iter, next } => {
                let v = acc!();
                let (i, n) = self.get_iterator(v, true)?;
                reg!(iter) = i;
                reg!(next) = n;
            }
            Instr::IteratorStep { iter, next, done } => {
                let i = reg!(iter);
                let n = reg!(next);
                match self.iterator_step_value(i, n)? {
                    Some(v) => set_acc!(v),
                    None => jump!(done),
                }
            }
            Instr::IteratorNextRaw { iter, next } => {
                let i = reg!(iter);
                let n = reg!(next);
                let r = self.call_value(n, i, &[])?;
                set_acc!(r);
            }
            Instr::IteratorResult { done } => {
                let r = acc!();
                let Value::Object(ro) = r else {
                    return Err(self.throw_type("Iterator result is not an object"));
                };
                let d = self.get_prop(ro, PropKey::Str(self.names.done), r)?;
                if self.to_boolean(d) {
                    jump!(done);
                } else {
                    let v = self.get_prop(ro, PropKey::Str(self.names.value), r)?;
                    set_acc!(v);
                }
            }
            Instr::IteratorClose { iter } => {
                let i = reg!(iter);
                let saved = acc!();
                self.iterator_close(i)?;
                set_acc!(saved);
            }
            Instr::IteratorCloseSwallow { iter } => {
                let i = reg!(iter);
                let saved = acc!();
                let _ = self.iterator_close(i);
                set_acc!(saved);
            }
            Instr::GetIteratorReturn { iter } => {
                let i = reg!(iter);
                let m = self.get_method(i, PropKey::Str(self.names.return_))?;
                set_acc!(m.unwrap_or(Value::Undefined));
            }
            Instr::ForInPrepare { dst } => {
                let v = acc!();
                let it = self.for_in_prepare(v)?;
                reg!(dst) = Value::Object(it);
            }
            Instr::ForInNext { iter, done } => {
                let it = reg!(iter).as_object().expect("ForInNext on a non-iterator");
                match self.for_in_next(it)? {
                    Some(k) => set_acc!(k),
                    None => jump!(done),
                }
            }

            // ---- generators and async ----
            Instr::InitialYield => {
                let gen = self.generator_initial_yield(fi)?;
                return Ok(Flow::Done(RunOutcome::Return(Value::Object(gen))));
            }
            Instr::Yield { kind } => {
                let v = acc!();
                self.suspend_frame(fi, Some(kind));
                return Ok(Flow::Done(RunOutcome::Yield(v)));
            }
            Instr::YieldRaw { kind } => {
                let v = acc!();
                self.suspend_frame(fi, Some(kind));
                return Ok(Flow::Done(RunOutcome::YieldRaw(v)));
            }
            Instr::Await => {
                let v = acc!();
                self.suspend_frame(fi, None);
                return Ok(Flow::Done(RunOutcome::Await(v)));
            }
            Instr::ResumeDispatch { kind, on_return } => {
                let k = reg!(kind);
                if let Value::Number(n) = k {
                    if n as i32 == RESUME_THROW {
                        return Err(Fault::Throw(acc!()));
                    }
                    if n as i32 == RESUME_RETURN {
                        jump!(on_return);
                    }
                }
            }
            Instr::Debugger | Instr::Nop => {}
        }
        Ok(Flow::Next)
    }

    /// `Return`: pop the frame and deliver the value.
    pub(crate) fn do_return(&mut self) -> Flow {
        let frame = self.pop_frame();
        let mut v = frame.acc;
        if frame.construct == super::ConstructKind::Base && !v.is_object() {
            v = frame.this;
        }
        if let Some(g) = frame.generator {
            crate::interp::generator::mark_completed(self, g);
        }
        if frame.exit {
            return Flow::Done(RunOutcome::Return(v));
        }
        self.frames
            .last_mut()
            .expect("return with no caller frame")
            .acc = v;
        Flow::Reload
    }

    /// The elements of an array built by the compiler (no holes).
    pub(crate) fn array_to_vec(&mut self, arr: crate::value::ObjId) -> Vec<Value> {
        if let Some(v) = self.heap.dense_values(arr) {
            return v.to_vec();
        }
        let len = self.heap.obj(arr).array_length().unwrap_or(0);
        let mut out = Vec::with_capacity(len as usize);
        for i in 0..len {
            out.push(
                self.heap
                    .get_own_property(arr, PropKey::Index(i))
                    .and_then(|p| p.value())
                    .unwrap_or(Value::Undefined),
            );
        }
        out
    }

    fn home_object_of(&mut self, f: Value) -> JsResult<crate::value::ObjId> {
        if let Value::Object(fo) = f {
            if let Some(fd) = self.heap.obj(fo).function() {
                if let Some(h) = fd.home_object {
                    return Ok(h);
                }
            }
        }
        Err(self.throw_syntax("'super' keyword unexpected here"))
    }

    /// Property access on any value with a nicer error for null/undefined.
    pub(crate) fn get_value_named(
        &mut self,
        o: Value,
        key: PropKey,
        name: &str,
    ) -> JsResult<Value> {
        if o.is_nullish() {
            let msg = format!(
                "Cannot read properties of {} (reading '{}')",
                if o.is_null() { "null" } else { "undefined" },
                name
            );
            return Err(self.throw_type(&msg));
        }
        self.get_value(o, key)
    }

    pub(crate) fn display_key(&mut self, k: PropKey) -> String {
        match k {
            PropKey::Index(i) => i.to_string(),
            PropKey::Str(s) => self.heap.str_to_string(s),
            PropKey::Sym(s) => match self.heap.sym(s).description {
                Some(d) => format!("Symbol({})", self.heap.str_to_string(d)),
                None => "Symbol()".into(),
            },
        }
    }

    fn display_key_value(&mut self, k: Value) -> JsResult<String> {
        Ok(match k {
            Value::Symbol(s) => self.display_key(PropKey::Sym(s)),
            Value::String(s) => self.heap.str_to_string(s),
            Value::Number(n) => gable_js_parser::number::to_js_string(n),
            Value::Object(_) => "[object]".into(),
            other => format!("{other:?}"),
        })
    }

    // ---- private elements ----

    pub(crate) fn private_get(&mut self, o: Value, name: crate::value::SymId) -> JsResult<Value> {
        let Value::Object(obj) = o else {
            return Err(self.throw_type("Cannot read private member from a non-object"));
        };
        let Some(p) = self.heap.get_own_property(obj, PropKey::Sym(name)) else {
            let d = self.private_display(name);
            return Err(self.throw_type(&format!(
                "Cannot read private member {d} from an object whose class did not declare it"
            )));
        };
        match p.slot {
            PropSlot::Data(v) => Ok(v),
            PropSlot::Accessor { get, .. } => {
                if get.is_undefined() {
                    let d = self.private_display(name);
                    return Err(self.throw_type(&format!("'{d}' was defined without a getter")));
                }
                self.call_value(get, o, &[])
            }
        }
    }

    pub(crate) fn private_set(
        &mut self,
        o: Value,
        name: crate::value::SymId,
        v: Value,
    ) -> JsResult<()> {
        let Value::Object(obj) = o else {
            return Err(self.throw_type("Cannot write private member to a non-object"));
        };
        let Some(p) = self.heap.get_own_property(obj, PropKey::Sym(name)) else {
            let d = self.private_display(name);
            return Err(self.throw_type(&format!(
                "Cannot write private member {d} to an object whose class did not declare it"
            )));
        };
        match p.slot {
            PropSlot::Data(_) => {
                if !p.attrs.writable() {
                    let d = self.private_display(name);
                    return Err(self.throw_type(&format!("Private method '{d}' is not writable")));
                }
                self.heap.set_own_value(obj, PropKey::Sym(name), v);
                Ok(())
            }
            PropSlot::Accessor { set, .. } => {
                if set.is_undefined() {
                    let d = self.private_display(name);
                    return Err(self.throw_type(&format!("'{d}' was defined without a setter")));
                }
                self.call_value(set, o, &[v])?;
                Ok(())
            }
        }
    }

    fn private_display(&mut self, name: crate::value::SymId) -> String {
        match self.heap.sym(name).description {
            Some(d) => self.heap.str_to_string(d),
            None => "#<private>".into(),
        }
    }

    /// PrivateMethodOrAccessorAdd.
    pub(crate) fn private_method_add(
        &mut self,
        obj: crate::value::ObjId,
        name: crate::value::SymId,
    ) -> JsResult<()> {
        if self.heap.has_own_property(obj, PropKey::Sym(name)) {
            let d = self.private_display(name);
            return Err(self.throw_type(&format!("Cannot initialize {d} twice on the same object")));
        }
        let p = match self.heap.sym(name).private {
            PrivateElement::Method(m) => Property::data(m, Attrs::NONE),
            PrivateElement::Accessor { get, set } => Property::accessor(get, set, Attrs::NONE),
            PrivateElement::Field => Property::data(Value::Undefined, Attrs::WRITABLE.into()),
        };
        self.heap.add_own_property(obj, PropKey::Sym(name), p);
        Ok(())
    }

    /// PrivateFieldAdd.
    pub(crate) fn private_field_add(
        &mut self,
        obj: crate::value::ObjId,
        name: crate::value::SymId,
        v: Value,
    ) -> JsResult<()> {
        if self.heap.has_own_property(obj, PropKey::Sym(name)) {
            let d = self.private_display(name);
            return Err(self.throw_type(&format!("Cannot initialize {d} twice on the same object")));
        }
        self.heap.add_own_property(
            obj,
            PropKey::Sym(name),
            Property::data(v, Attrs(Attrs::WRITABLE)),
        );
        Ok(())
    }

    /// DefineField with the anonymous-function naming rule.
    pub(crate) fn define_field(
        &mut self,
        obj: crate::value::ObjId,
        key: &FieldKey,
        v: Value,
        anon: bool,
    ) -> JsResult<()> {
        if anon {
            if let Value::Object(f) = v {
                let name = match key {
                    FieldKey::Prop(pk) => self.key_to_value(*pk),
                    FieldKey::Private(s) => self
                        .heap
                        .sym(*s)
                        .description
                        .map(Value::String)
                        .unwrap_or(Value::Undefined),
                };
                self.set_function_name(f, name, None);
            }
        }
        match key {
            FieldKey::Prop(pk) => {
                self.create_data_property_or_throw(obj, *pk, v)?;
            }
            FieldKey::Private(s) => self.private_field_add(obj, *s, v)?,
        }
        Ok(())
    }

    /// InitializeInstanceElements(O, constructor).
    pub(crate) fn initialize_instance_elements(
        &mut self,
        ctor: crate::value::ObjId,
        this: crate::value::ObjId,
    ) -> JsResult<()> {
        let (methods, fields) = match self
            .heap
            .obj(ctor)
            .function()
            .and_then(|f| f.class_data.as_ref())
        {
            Some(cd) => (cd.private_methods.clone(), cd.fields.clone()),
            None => return Ok(()),
        };
        for m in methods {
            self.private_method_add(this, m)?;
        }
        for f in fields {
            let v = match f.init {
                Some(init) => self.call_value(Value::Object(init), Value::Object(this), &[])?,
                None => Value::Undefined,
            };
            self.define_field(this, &f.key, v, f.anon)?;
        }
        Ok(())
    }

    /// GetTemplateObject, cached per realm and site.
    pub(crate) fn get_template_object(
        &mut self,
        code: &Rc<CodeBlock>,
        c: u32,
    ) -> crate::value::ObjId {
        let key = (Rc::as_ptr(code) as usize, c);
        if let Some((_, o)) = self.realm().template_cache.get(&key) {
            return *o;
        }
        let Constant::Template(t) = &code.constants[c as usize] else {
            panic!("GetTemplateObject on a non-template constant");
        };
        let t = t.clone();
        let cooked: Vec<Value> = t
            .cooked
            .iter()
            .map(|c| match c {
                Some(s) => Value::String(self.heap.alloc_str_utf16(s.to_vec())),
                None => Value::Undefined,
            })
            .collect();
        let raw: Vec<Value> = t
            .raw
            .iter()
            .map(|s| Value::String(self.heap.alloc_str_utf16(s.to_vec())))
            .collect();
        let raw_arr = self.new_array(raw);
        self.set_integrity_frozen(raw_arr);
        let arr = self.new_array(cooked);
        let raw_key = PropKey::Str(self.names.raw);
        self.heap.add_own_property(
            arr,
            raw_key,
            Property::data(Value::Object(raw_arr), Attrs::NONE),
        );
        self.set_integrity_frozen(arr);
        self.realm_mut()
            .template_cache
            .insert(key, (code.clone(), arr));
        arr
    }

    /// Freeze an ordinary object or array (no proxies involved).
    pub(crate) fn set_integrity_frozen(&mut self, obj: crate::value::ObjId) {
        let keys = self.heap.own_keys(obj);
        for k in keys {
            if let Some(p) = self.heap.get_own_property(obj, k) {
                let attrs = p.attrs.with_configurable(false).with_writable(false);
                self.heap.replace_own_property(
                    obj,
                    k,
                    Property {
                        slot: p.slot,
                        attrs,
                    },
                );
            }
        }
        if let Class::Array(a) = &mut self.heap.obj_mut(obj).class {
            a.length_writable = false;
        }
        self.heap.obj_mut(obj).extensible = false;
    }

    /// CreateUnmappedArgumentsObject / CreateMappedArgumentsObject.
    pub(crate) fn create_arguments_object(
        &mut self,
        fi: usize,
        code: &Rc<CodeBlock>,
        mapped: bool,
    ) -> crate::value::ObjId {
        let args = self.frames[fi].args.clone();
        let proto = self.intrinsic(crate::realm::Intrinsic::ObjectPrototype);
        let map = if mapped {
            let env = self.frames[fi]
                .env
                .expect("mapped arguments without an environment");
            let info = self.heap.env(env).scope_info().cloned();
            let mut slots: Vec<Option<u16>> = vec![None; args.len()];
            if let Some(info) = info {
                // The last parameter with a given name wins.
                for (i, n) in code.param_names.iter().enumerate() {
                    if i >= args.len() {
                        break;
                    }
                    let last = code.param_names.iter().rposition(|m| m == n).unwrap();
                    if last == i {
                        slots[i] = info.slot_of(n).map(|s| s as u16);
                    }
                }
            }
            Some(Box::new(crate::object::MappedArgs { env, map: slots }))
        } else {
            None
        };
        let obj = self.alloc(ObjectData::new(Some(proto), Class::Arguments(map)));
        for (i, a) in args.iter().enumerate() {
            self.heap.add_own_property(
                obj,
                PropKey::Index(i as u32),
                Property::data(*a, Attrs::ALL),
            );
        }
        let len_key = PropKey::Str(self.names.length);
        self.heap.add_own_property(
            obj,
            len_key,
            Property::data(Value::from_usize(args.len()), Attrs::METHOD),
        );
        let values = self.intrinsic(crate::realm::Intrinsic::ArrayPrototypeValues);
        self.heap.add_own_property(
            obj,
            PropKey::Sym(self.well_known.iterator),
            Property::data(Value::Object(values), Attrs::METHOD),
        );
        if mapped {
            let callee = self.frames[fi]
                .callee
                .map(Value::Object)
                .unwrap_or(Value::Undefined);
            let ck = PropKey::Str(self.names.callee);
            self.heap
                .add_own_property(obj, ck, Property::data(callee, Attrs::METHOD));
        } else {
            let thrower = Value::Object(self.intrinsic(crate::realm::Intrinsic::ThrowTypeError));
            let ck = PropKey::Str(self.names.callee);
            self.heap
                .add_own_property(obj, ck, Property::accessor(thrower, thrower, Attrs::NONE));
        }
        obj
    }

    pub(crate) fn hint_for(&self) -> Hint {
        Hint::Default
    }
}

/// Strip the compiler's pseudo-binding decoration for messages.
fn display_name(n: &str) -> &str {
    match n {
        "*this*" => "this",
        "*new.target*" => "new.target",
        other => other,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArithOp {
    Sub,
    Mul,
    Div,
    Mod,
    Exp,
    Shl,
    Shr,
    UShr,
    BitAnd,
    BitOr,
    BitXor,
}

fn instr_arith(i: Instr) -> ArithOp {
    match i {
        Instr::Sub(_) => ArithOp::Sub,
        Instr::Mul(_) => ArithOp::Mul,
        Instr::Div(_) => ArithOp::Div,
        Instr::Mod(_) => ArithOp::Mod,
        Instr::Exp(_) => ArithOp::Exp,
        Instr::Shl(_) => ArithOp::Shl,
        Instr::Shr(_) => ArithOp::Shr,
        Instr::UShr(_) => ArithOp::UShr,
        Instr::BitAnd(_) => ArithOp::BitAnd,
        Instr::BitOr(_) => ArithOp::BitOr,
        Instr::BitXor(_) => ArithOp::BitXor,
        _ => unreachable!(),
    }
}

impl Value {
    pub(crate) fn as_symbol_id(self) -> crate::value::SymId {
        match self {
            Value::Symbol(s) => s,
            other => panic!("expected a private name, found {other:?}"),
        }
    }
}

impl From<u8> for Attrs {
    fn from(b: u8) -> Attrs {
        Attrs(b)
    }
}
