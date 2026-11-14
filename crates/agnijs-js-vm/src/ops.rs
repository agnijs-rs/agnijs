//! Object internal methods ([[Get]], [[Set]], [[DefineOwnProperty]], ...)
//! with the exotic behaviours of arrays, strings, arguments objects,
//! typed arrays, module namespaces and proxies, plus the abstract
//! operations built on them.

use crate::builtins::{proxy, typedarray};
use crate::object::{ArrayData, Class, Elements, ForInState, ObjectData};
use crate::realm::Intrinsic;
use crate::value::{Attrs, ObjId, PropKey, PropSlot, Property, PropertyDescriptor, Value};
use crate::vm::{JsResult, Vm};

/// The longest list built from a length a script chose: the argument list
/// of `apply`, `Reflect.apply` and `Reflect.construct`, a Proxy `ownKeys`
/// result, the captures of a custom `exec` result. The specification has
/// no maximum; engines refuse far shorter argument lists (V8 about 120k).
/// A dense array is copied as it is, whatever its length: its memory
/// already exists and is already counted.
pub const MAX_LIST_LENGTH: usize = 1 << 22;

/// The result of [`Vm::ordinary_get`].
pub(crate) enum OrdinaryGet {
    Value(Value),
    Getter(ObjId),
    Generic,
}

/// Which exotic behaviour an object has, decided once per operation.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Exotic {
    None,
    Array,
    String,
    MappedArguments,
    TypedArray,
    Namespace,
    Proxy,
}

impl Vm {
    #[inline]
    fn exotic_of(&self, o: ObjId) -> Exotic {
        match &self.heap.obj(o).class {
            Class::Array(_) => Exotic::Array,
            Class::String(_) => Exotic::String,
            Class::Arguments(Some(_)) => Exotic::MappedArguments,
            Class::TypedArray(_) => Exotic::TypedArray,
            Class::Namespace(_) => Exotic::Namespace,
            Class::Proxy(_) => Exotic::Proxy,
            _ => Exotic::None,
        }
    }

    // ---- [[GetPrototypeOf]] / [[SetPrototypeOf]] / extensibility ----------------------

    pub fn get_prototype_of(&mut self, o: ObjId) -> JsResult<Option<ObjId>> {
        if self.heap.obj(o).is_proxy() {
            return proxy::proxy_get_prototype_of(self, o);
        }
        Ok(self.heap.obj(o).proto())
    }

    pub fn set_prototype_of(&mut self, o: ObjId, proto: Option<ObjId>) -> JsResult<bool> {
        match self.exotic_of(o) {
            Exotic::Proxy => return proxy::proxy_set_prototype_of(self, o, proto),
            Exotic::Namespace => return Ok(proto.is_none()),
            _ => {}
        }
        Ok(self.ordinary_set_prototype_of(o, proto))
    }

    pub fn ordinary_set_prototype_of(&mut self, o: ObjId, proto: Option<ObjId>) -> bool {
        let cur = self.heap.obj(o).proto();
        if cur == proto {
            return true;
        }
        if !self.heap.obj(o).extensible {
            return false;
        }
        // Immutable prototype exotic: %Object.prototype%.
        let realm_op = self.realm().intrinsic(Intrinsic::ObjectPrototype);
        if o == realm_op
            || self
                .realms
                .iter()
                .any(|r| r.intrinsic(Intrinsic::ObjectPrototype) == o)
        {
            return false;
        }
        let mut p = proto;
        while let Some(pp) = p {
            if pp == o {
                return false;
            }
            if self.heap.obj(pp).is_proxy() {
                break;
            }
            p = self.heap.obj(pp).proto();
        }
        self.heap.set_proto(o, proto);
        true
    }

    pub fn is_extensible(&mut self, o: ObjId) -> JsResult<bool> {
        if self.heap.obj(o).is_proxy() {
            return proxy::proxy_is_extensible(self, o);
        }
        Ok(self.heap.obj(o).extensible)
    }

    pub fn prevent_extensions(&mut self, o: ObjId) -> JsResult<bool> {
        match self.exotic_of(o) {
            Exotic::Proxy => return proxy::proxy_prevent_extensions(self, o),
            Exotic::TypedArray => {
                if !typedarray::is_fixed_length(self, o) {
                    return Ok(false);
                }
            }
            _ => {}
        }
        self.heap.obj_mut(o).extensible = false;
        Ok(true)
    }

    // ---- [[GetOwnProperty]] -----------------------------------------------------------

    /// The own property of `o` named `key`, including exotic virtual ones.
    pub fn own_property(&mut self, o: ObjId, key: PropKey) -> JsResult<Option<Property>> {
        match self.exotic_of(o) {
            Exotic::None => Ok(self.heap.get_own_property(o, key)),
            Exotic::Array => {
                if key == PropKey::Str(self.names.length) {
                    let Class::Array(a) = &self.heap.obj(o).class else {
                        unreachable!()
                    };
                    return Ok(Some(Property::data(
                        Value::Number(a.length as f64),
                        Attrs::new(a.length_writable, false, false),
                    )));
                }
                Ok(self.heap.get_own_property(o, key))
            }
            Exotic::String => {
                let Class::String(s) = &self.heap.obj(o).class else {
                    unreachable!()
                };
                let s = *s;
                if let PropKey::Index(i) = key {
                    let len = self.heap.str_len(s);
                    if (i as usize) < len {
                        let unit = self.heap.units(s).at(i as usize);
                        let ch = self.heap.char_str(unit);
                        return Ok(Some(Property::data(
                            Value::String(ch),
                            Attrs::new(false, true, false),
                        )));
                    }
                }
                Ok(self.heap.get_own_property(o, key))
            }
            Exotic::MappedArguments => {
                let p = self.heap.get_own_property(o, key);
                if let (Some(mut p), PropKey::Index(i)) = (p, key) {
                    if let Some(v) = self.mapped_arg_get(o, i) {
                        p.slot = PropSlot::Data(v);
                    }
                    return Ok(Some(p));
                }
                Ok(p)
            }
            Exotic::TypedArray => typedarray::ta_get_own_property(self, o, key),
            Exotic::Namespace => crate::modules::ns_get_own_property(self, o, key),
            Exotic::Proxy => proxy::proxy_get_own_property(self, o, key),
        }
    }

    pub fn get_own_property_descriptor(
        &mut self,
        o: ObjId,
        key: PropKey,
    ) -> JsResult<Option<PropertyDescriptor>> {
        Ok(self
            .own_property(o, key)?
            .map(|p| PropertyDescriptor::from_property(&p)))
    }

    /// The value of a mapped argument, if `i` is still mapped.
    fn mapped_arg_get(&self, o: ObjId, i: u32) -> Option<Value> {
        let Class::Arguments(Some(m)) = &self.heap.obj(o).class else {
            return None;
        };
        let slot = (*m.map.get(i as usize)?)?;
        Some(self.heap.env(m.env).as_decl()?.slots[slot as usize])
    }

    fn mapped_arg_set(&mut self, o: ObjId, i: u32, v: Value) -> bool {
        let Class::Arguments(Some(m)) = &self.heap.obj(o).class else {
            return false;
        };
        let Some(Some(slot)) = m.map.get(i as usize) else {
            return false;
        };
        let env = m.env;
        let slot = *slot;
        if let Some(d) = self.heap.env_mut(env).as_decl_mut() {
            d.slots[slot as usize] = v;
        }
        true
    }

    fn mapped_arg_unmap(&mut self, o: ObjId, i: u32) {
        if let Class::Arguments(Some(m)) = &mut self.heap.obj_mut(o).class {
            if let Some(s) = m.map.get_mut(i as usize) {
                *s = None;
            }
        }
    }

    // ---- [[DefineOwnProperty]] ------------------------------------------------------

    pub fn define_own_property(
        &mut self,
        o: ObjId,
        key: PropKey,
        desc: PropertyDescriptor,
    ) -> JsResult<bool> {
        match self.exotic_of(o) {
            Exotic::None => Ok(self.ordinary_define_own_property(o, key, desc)),
            Exotic::Array => self.array_define_own_property(o, key, desc),
            Exotic::String => {
                if let PropKey::Index(i) = key {
                    let Class::String(s) = &self.heap.obj(o).class else {
                        unreachable!()
                    };
                    if (i as usize) < self.heap.str_len(*s) {
                        let cur = self.own_property(o, key)?.unwrap();
                        return Ok(self.validate_and_apply(None, key, false, &desc, Some(cur)));
                    }
                }
                Ok(self.ordinary_define_own_property(o, key, desc))
            }
            Exotic::MappedArguments => {
                let PropKey::Index(i) = key else {
                    return Ok(self.ordinary_define_own_property(o, key, desc));
                };
                let mapped = self.mapped_arg_get(o, i);
                let mut new_desc = desc.clone();
                if let Some(cur) = mapped {
                    if desc.is_data() && desc.value.is_none() && desc.writable == Some(false) {
                        new_desc.value = Some(cur);
                    }
                }
                if !self.ordinary_define_own_property(o, key, new_desc) {
                    return Ok(false);
                }
                if mapped.is_some() {
                    if desc.is_accessor() {
                        self.mapped_arg_unmap(o, i);
                    } else {
                        if let Some(v) = desc.value {
                            self.mapped_arg_set(o, i, v);
                        }
                        if desc.writable == Some(false) {
                            self.mapped_arg_unmap(o, i);
                        }
                    }
                }
                Ok(true)
            }
            Exotic::TypedArray => typedarray::ta_define_own_property(self, o, key, desc),
            Exotic::Namespace => crate::modules::ns_define_own_property(self, o, key, desc),
            Exotic::Proxy => proxy::proxy_define_own_property(self, o, key, desc),
        }
    }

    pub fn ordinary_define_own_property(
        &mut self,
        o: ObjId,
        key: PropKey,
        desc: PropertyDescriptor,
    ) -> bool {
        let current = self.heap.get_own_property(o, key);
        let extensible = self.heap.obj(o).extensible;
        self.validate_and_apply(Some(o), key, extensible, &desc, current)
    }

    /// ValidateAndApplyPropertyDescriptor.
    pub fn validate_and_apply(
        &mut self,
        o: Option<ObjId>,
        key: PropKey,
        extensible: bool,
        desc: &PropertyDescriptor,
        current: Option<Property>,
    ) -> bool {
        let Some(cur) = current else {
            if !extensible {
                return false;
            }
            if let Some(o) = o {
                let prop = if desc.is_accessor() {
                    Property::accessor(
                        desc.get.unwrap_or(Value::Undefined),
                        desc.set.unwrap_or(Value::Undefined),
                        Attrs::new(
                            false,
                            desc.enumerable.unwrap_or(false),
                            desc.configurable.unwrap_or(false),
                        ),
                    )
                } else {
                    Property::data(
                        desc.value.unwrap_or(Value::Undefined),
                        Attrs::new(
                            desc.writable.unwrap_or(false),
                            desc.enumerable.unwrap_or(false),
                            desc.configurable.unwrap_or(false),
                        ),
                    )
                };
                self.heap.add_own_property(o, key, prop);
            }
            return true;
        };
        if desc.is_generic() && desc.enumerable.is_none() && desc.configurable.is_none() {
            return true;
        }
        if !cur.attrs.configurable() {
            if desc.configurable == Some(true) {
                return false;
            }
            if let Some(e) = desc.enumerable {
                if e != cur.attrs.enumerable() {
                    return false;
                }
            }
            if !desc.is_generic() && desc.is_accessor() != cur.is_accessor() {
                return false;
            }
            match cur.slot {
                PropSlot::Accessor { get, set } => {
                    if let Some(g) = desc.get {
                        if !self.same_value(g, get.value()) {
                            return false;
                        }
                    }
                    if let Some(s) = desc.set {
                        if !self.same_value(s, set.value()) {
                            return false;
                        }
                    }
                }
                PropSlot::Data(v) => {
                    if !cur.attrs.writable() {
                        if desc.writable == Some(true) {
                            return false;
                        }
                        if let Some(nv) = desc.value {
                            if !self.same_value(nv, v) {
                                return false;
                            }
                        }
                    }
                }
            }
        }
        let Some(o) = o else { return true };
        let enumerable = desc.enumerable.unwrap_or(cur.attrs.enumerable());
        let configurable = desc.configurable.unwrap_or(cur.attrs.configurable());
        let new = if desc.is_accessor() && !cur.is_accessor() {
            Property::accessor(
                desc.get.unwrap_or(Value::Undefined),
                desc.set.unwrap_or(Value::Undefined),
                Attrs::new(false, enumerable, configurable),
            )
        } else if desc.is_data() && cur.is_accessor() {
            Property::data(
                desc.value.unwrap_or(Value::Undefined),
                Attrs::new(desc.writable.unwrap_or(false), enumerable, configurable),
            )
        } else {
            match cur.slot {
                PropSlot::Data(v) => Property::data(
                    desc.value.unwrap_or(v),
                    Attrs::new(
                        desc.writable.unwrap_or(cur.attrs.writable()),
                        enumerable,
                        configurable,
                    ),
                ),
                PropSlot::Accessor { get, set } => Property::accessor(
                    desc.get.unwrap_or(get.value()),
                    desc.set.unwrap_or(set.value()),
                    Attrs::new(false, enumerable, configurable),
                ),
            }
        };
        self.heap.replace_own_property(o, key, new);
        true
    }

    /// Array [[DefineOwnProperty]] (ArraySetLength for "length").
    fn array_define_own_property(
        &mut self,
        o: ObjId,
        key: PropKey,
        desc: PropertyDescriptor,
    ) -> JsResult<bool> {
        if key == PropKey::Str(self.names.length) {
            return self.array_set_length(o, desc);
        }
        if let PropKey::Index(i) = key {
            let (len, len_writable) = {
                let Class::Array(a) = &self.heap.obj(o).class else {
                    unreachable!()
                };
                (a.length, a.length_writable)
            };
            if i >= len && !len_writable {
                return Ok(false);
            }
            if !self.ordinary_define_own_property(o, key, desc) {
                return Ok(false);
            }
            if i >= len {
                if let Class::Array(a) = &mut self.heap.obj_mut(o).class {
                    a.length = i + 1;
                }
            }
            return Ok(true);
        }
        Ok(self.ordinary_define_own_property(o, key, desc))
    }

    fn array_set_length(&mut self, o: ObjId, desc: PropertyDescriptor) -> JsResult<bool> {
        let (old_len, writable) = {
            let Class::Array(a) = &self.heap.obj(o).class else {
                unreachable!()
            };
            (a.length, a.length_writable)
        };
        let Some(v) = desc.value else {
            // Generic / attribute-only change on length.
            let cur = Property::data(
                Value::Number(old_len as f64),
                Attrs::new(writable, false, false),
            );
            if !self.validate_and_apply(
                None,
                PropKey::Str(self.names.length),
                false,
                &desc,
                Some(cur),
            ) {
                return Ok(false);
            }
            if desc.writable == Some(false) {
                if let Class::Array(a) = &mut self.heap.obj_mut(o).class {
                    a.length_writable = false;
                }
            }
            return Ok(true);
        };
        let new_len = self.to_uint32(v)?;
        let number_len = self.to_number(v)?;
        if new_len as f64 != number_len {
            return Err(self.throw_range("Invalid array length"));
        }
        // Step 8: the coercions above may have changed the length or made
        // it non-writable.
        let (old_len, writable) = {
            let Class::Array(a) = &self.heap.obj(o).class else {
                unreachable!()
            };
            (a.length, a.length_writable)
        };
        let cur = Property::data(
            Value::Number(old_len as f64),
            Attrs::new(writable, false, false),
        );
        let mut check = desc.clone();
        check.value = Some(Value::Number(new_len as f64));
        if !self.validate_and_apply(
            None,
            PropKey::Str(self.names.length),
            false,
            &check,
            Some(cur),
        ) {
            return Ok(false);
        }
        if new_len >= old_len {
            if let Class::Array(a) = &mut self.heap.obj_mut(o).class {
                a.length = new_len;
                if desc.writable == Some(false) {
                    a.length_writable = false;
                }
            }
            return Ok(true);
        }
        if !writable {
            return Ok(false);
        }
        let stuck = self.heap.truncate_elements(o, new_len);
        let final_len = match stuck {
            Some(i) => i + 1,
            None => new_len,
        };
        if let Class::Array(a) = &mut self.heap.obj_mut(o).class {
            a.length = final_len;
            if desc.writable == Some(false) {
                a.length_writable = false;
            }
        }
        Ok(stuck.is_none())
    }

    // ---- [[HasProperty]] ---------------------------------------------------------------

    pub fn has_property(&mut self, o: ObjId, key: PropKey) -> JsResult<bool> {
        let mut cur = o;
        loop {
            match self.exotic_of(cur) {
                Exotic::Proxy => return proxy::proxy_has(self, cur, key),
                Exotic::Namespace => return Ok(crate::modules::ns_has_property(self, cur, key)),
                Exotic::TypedArray => {
                    if let Some(r) = typedarray::ta_has_property(self, cur, key)? {
                        return Ok(r);
                    }
                    if self.heap.has_own_property(cur, key) {
                        return Ok(true);
                    }
                }
                Exotic::None => {
                    if self.heap.has_own_property(cur, key) {
                        return Ok(true);
                    }
                }
                _ => {
                    if self.own_property(cur, key)?.is_some() {
                        return Ok(true);
                    }
                }
            }
            match self.heap.obj(cur).proto() {
                Some(p) => cur = p,
                None => return Ok(false),
            }
        }
    }

    pub fn has_own_property(&mut self, o: ObjId, key: PropKey) -> JsResult<bool> {
        Ok(self.own_property(o, key)?.is_some())
    }

    // ---- [[Get]] -------------------------------------------------------------------------

    pub fn get_prop(&mut self, o: ObjId, key: PropKey, receiver: Value) -> JsResult<Value> {
        let mut cur = o;
        loop {
            let prop = match self.exotic_of(cur) {
                Exotic::None => self.heap.get_own_property(cur, key),
                Exotic::Proxy => return proxy::proxy_get(self, cur, key, receiver),
                Exotic::TypedArray => {
                    if let Some(r) = typedarray::ta_get(self, cur, key)? {
                        return Ok(r);
                    }
                    self.heap.get_own_property(cur, key)
                }
                Exotic::Namespace => return crate::modules::ns_get(self, cur, key, receiver),
                _ => self.own_property(cur, key)?,
            };
            if let Some(p) = prop {
                return match p.slot {
                    PropSlot::Data(v) => Ok(v),
                    PropSlot::Accessor { get, .. } => {
                        if get.is_undefined() {
                            Ok(Value::Undefined)
                        } else {
                            self.call_value(get.value(), receiver, &[])
                        }
                    }
                };
            }
            match self.heap.obj(cur).proto() {
                Some(p) => cur = p,
                None => return Ok(Value::Undefined),
            }
        }
    }

    /// [[Get]] of `key` on `o` (the receiver) when every object on the way
    /// to the holder is ordinary, so the lookup observes nothing
    /// (TASK-100.19): the data value, or the bytecode getter to push as a
    /// frame in the current run loop. `Generic` sends the access down
    /// `get_prop`, which repeats the lookup with all of its effects.
    pub(crate) fn ordinary_get(&self, o: ObjId, key: PropKey) -> OrdinaryGet {
        let mut cur = o;
        loop {
            if self.exotic_of(cur) != Exotic::None {
                return OrdinaryGet::Generic;
            }
            if let Some(p) = self.heap.get_own_property(cur, key) {
                return match p.slot {
                    PropSlot::Data(v) => OrdinaryGet::Value(v),
                    PropSlot::Accessor { get, .. } if get.is_undefined() => {
                        OrdinaryGet::Value(Value::Undefined)
                    }
                    PropSlot::Accessor { get, .. } => match self.inlinable_callee(get.value()) {
                        Some(g) => OrdinaryGet::Getter(g),
                        None => OrdinaryGet::Generic,
                    },
                };
            }
            match self.heap.obj(cur).proto() {
                Some(p) => cur = p,
                None => return OrdinaryGet::Value(Value::Undefined),
            }
        }
    }

    /// Get(O, P) with O an object value.
    pub fn get(&mut self, o: ObjId, key: PropKey) -> JsResult<Value> {
        self.get_prop(o, key, Value::Object(o))
    }

    pub fn get_str(&mut self, o: ObjId, name: &str) -> JsResult<Value> {
        let k = self.key(name);
        self.get_prop(o, k, Value::Object(o))
    }

    /// GetV: property lookup on any value (primitives use their prototype).
    pub fn get_value(&mut self, v: Value, key: PropKey) -> JsResult<Value> {
        match v {
            Value::Object(o) => self.get_prop(o, key, v),
            Value::String(s) => {
                // Fast path: string indices and length.
                match key {
                    PropKey::Index(i) => {
                        let len = self.heap.str_len(s);
                        if (i as usize) < len {
                            let unit = self.heap.units(s).at(i as usize);
                            return Ok(Value::String(self.heap.char_str(unit)));
                        }
                    }
                    PropKey::Str(n) if n == self.names.length => {
                        return Ok(Value::from_usize(self.heap.str_len(s)));
                    }
                    _ => {}
                }
                let proto = self.intrinsic(Intrinsic::StringPrototype);
                self.get_prop(proto, key, v)
            }
            Value::Number(_) => {
                let proto = self.intrinsic(Intrinsic::NumberPrototype);
                self.get_prop(proto, key, v)
            }
            Value::Bool(_) => {
                let proto = self.intrinsic(Intrinsic::BooleanPrototype);
                self.get_prop(proto, key, v)
            }
            Value::Symbol(_) => {
                let proto = self.intrinsic(Intrinsic::SymbolPrototype);
                self.get_prop(proto, key, v)
            }
            Value::BigInt(_) => {
                let proto = self.intrinsic(Intrinsic::BigIntPrototype);
                self.get_prop(proto, key, v)
            }
            Value::Undefined | Value::Null | Value::Empty => {
                let ks = self.display_key(key);
                let msg = format!(
                    "Cannot read properties of {} (reading '{}')",
                    if v.is_null() { "null" } else { "undefined" },
                    ks
                );
                Err(self.throw_type(&msg))
            }
        }
    }

    // ---- [[Set]] -----------------------------------------------------------------------

    pub fn set_prop(
        &mut self,
        o: ObjId,
        key: PropKey,
        v: Value,
        receiver: Value,
    ) -> JsResult<bool> {
        match self.set_prop_with(o, key, v, receiver, false)? {
            Ok(ok) => Ok(ok),
            Err(_) => unreachable!("setter returned with inline_setter off"),
        }
    }

    /// [[Set]], except that with `inline_setter` a bytecode setter that
    /// can be a frame push in the current run loop is returned (`Err`)
    /// instead of called (TASK-100.19). Everything before the call (the
    /// lookup and its effects) has then happened.
    #[inline]
    pub(crate) fn set_prop_with(
        &mut self,
        o: ObjId,
        key: PropKey,
        v: Value,
        receiver: Value,
        inline_setter: bool,
    ) -> JsResult<Result<bool, ObjId>> {
        // Fast path: own writable data property on an ordinary receiver.
        if receiver == Value::Object(o) && self.exotic_of(o) == Exotic::None {
            if let Some(p) = self.heap.get_own_property(o, key) {
                if let PropSlot::Data(_) = p.slot {
                    if p.attrs.writable() {
                        self.heap.set_own_value(o, key, v);
                        return Ok(Ok(true));
                    }
                    return Ok(Ok(false));
                }
            }
        }
        let mut cur = o;
        let own_desc: Property = loop {
            match self.exotic_of(cur) {
                Exotic::Proxy => return proxy::proxy_set(self, cur, key, v, receiver).map(Ok),
                Exotic::TypedArray => {
                    if let Some(r) = typedarray::ta_set(self, cur, key, v, receiver)? {
                        return Ok(Ok(r));
                    }
                    // A valid element index seen through another receiver is
                    // an own writable data property.
                    if let Some(p) = self.own_property(cur, key)? {
                        break p;
                    }
                }
                // A module namespace's [[Set]] is "return false".
                Exotic::Namespace => return Ok(Ok(false)),
                Exotic::None => {
                    if let Some(p) = self.heap.get_own_property(cur, key) {
                        break p;
                    }
                }
                _ => {
                    if let Some(p) = self.own_property(cur, key)? {
                        break p;
                    }
                }
            }
            match self.heap.obj(cur).proto() {
                Some(p) => cur = p,
                None => break Property::data(Value::Undefined, Attrs::ALL),
            }
        };
        match own_desc.slot {
            PropSlot::Data(_) => {
                if !own_desc.attrs.writable() {
                    return Ok(Ok(false));
                }
                let Value::Object(r) = receiver else {
                    return Ok(Ok(false));
                };
                if let Some(existing) = self.own_property(r, key)? {
                    if existing.is_accessor() || !existing.attrs.writable() {
                        return Ok(Ok(false));
                    }
                    let desc = PropertyDescriptor {
                        value: Some(v),
                        ..Default::default()
                    };
                    self.define_own_property(r, key, desc).map(Ok)
                } else {
                    self.create_data_property(r, key, v).map(Ok)
                }
            }
            PropSlot::Accessor { set, .. } => {
                if set.is_undefined() {
                    return Ok(Ok(false));
                }
                if inline_setter {
                    if let Some(s) = self.inlinable_callee(set.value()) {
                        return Ok(Err(s));
                    }
                }
                self.call_value(set.value(), receiver, &[v])?;
                Ok(Ok(true))
            }
        }
    }

    pub fn set(&mut self, o: ObjId, key: PropKey, v: Value, throw: bool) -> JsResult<()> {
        let ok = self.set_prop(o, key, v, Value::Object(o))?;
        if !ok && throw {
            let ks = self.display_key(key);
            return Err(self.throw_type(&format!(
                "Cannot assign to read only property '{ks}' of object"
            )));
        }
        Ok(())
    }

    pub fn set_str(&mut self, o: ObjId, name: &str, v: Value, throw: bool) -> JsResult<()> {
        let k = self.key(name);
        self.set(o, k, v, throw)
    }

    /// PutValue for a property reference `base[key] = v`.
    pub fn put_value(&mut self, base: Value, key: PropKey, v: Value, strict: bool) -> JsResult<()> {
        let ok = match base {
            Value::Object(o) => self.set_prop(o, key, v, base)?,
            Value::Undefined | Value::Null | Value::Empty => {
                let ks = self.display_key(key);
                let msg = format!(
                    "Cannot set properties of {} (setting '{}')",
                    if base.is_null() { "null" } else { "undefined" },
                    ks
                );
                return Err(self.throw_type(&msg));
            }
            _ => {
                let o = self.to_object(base)?;
                self.set_prop(o, key, v, base)?
            }
        };
        if !ok && strict {
            return Err(self.throw_read_only(key));
        }
        Ok(())
    }

    /// The TypeError for a failed strict-mode assignment to `key`.
    pub(crate) fn throw_read_only(&mut self, key: PropKey) -> crate::vm::Fault {
        let ks = self.display_key(key);
        self.throw_type(&format!("Cannot assign to read only property '{ks}' of object"))
    }

    // ---- [[Delete]] ---------------------------------------------------------------------

    pub fn delete_property(&mut self, o: ObjId, key: PropKey) -> JsResult<bool> {
        match self.exotic_of(o) {
            Exotic::Proxy => return proxy::proxy_delete(self, o, key),
            Exotic::TypedArray => {
                if let Some(r) = typedarray::ta_delete(self, o, key)? {
                    return Ok(r);
                }
            }
            Exotic::Namespace => return crate::modules::ns_delete(self, o, key),
            Exotic::Array => {
                if key == PropKey::Str(self.names.length) {
                    return Ok(false);
                }
            }
            Exotic::String => {
                if let PropKey::Index(i) = key {
                    let Class::String(s) = &self.heap.obj(o).class else {
                        unreachable!()
                    };
                    if (i as usize) < self.heap.str_len(*s) {
                        return Ok(false);
                    }
                }
            }
            Exotic::MappedArguments => {
                if let PropKey::Index(i) = key {
                    let r = self.ordinary_delete(o, key);
                    if r {
                        self.mapped_arg_unmap(o, i);
                    }
                    return Ok(r);
                }
            }
            Exotic::None => {}
        }
        Ok(self.ordinary_delete(o, key))
    }

    pub(crate) fn ordinary_delete(&mut self, o: ObjId, key: PropKey) -> bool {
        match self.heap.get_own_property(o, key) {
            None => true,
            Some(p) => {
                if !p.attrs.configurable() {
                    return false;
                }
                self.heap.remove_own_property(o, key);
                // Cached global property positions are stale now.
                if let Some(r) = self.realms.iter_mut().find(|r| r.global == o) {
                    r.epoch = r.epoch.wrapping_add(1);
                }
                true
            }
        }
    }

    pub fn delete_property_or_throw(&mut self, o: ObjId, key: PropKey) -> JsResult<()> {
        if !self.delete_property(o, key)? {
            let ks = self.display_key(key);
            return Err(self.throw_type(&format!("Cannot delete property '{ks}' of object")));
        }
        Ok(())
    }

    /// `delete base[key]`.
    pub fn delete_value(&mut self, base: Value, key: PropKey, strict: bool) -> JsResult<bool> {
        let o = match base {
            Value::Object(o) => o,
            Value::Undefined | Value::Null | Value::Empty => {
                return Err(self.throw_type("Cannot convert undefined or null to object"))
            }
            _ => self.to_object(base)?,
        };
        let r = self.delete_property(o, key)?;
        if !r && strict {
            let ks = self.display_key(key);
            return Err(self.throw_type(&format!("Cannot delete property '{ks}' of object")));
        }
        Ok(r)
    }

    // ---- [[OwnPropertyKeys]] ---------------------------------------------------------------

    pub fn own_property_keys(&mut self, o: ObjId) -> JsResult<Vec<PropKey>> {
        match self.exotic_of(o) {
            Exotic::None | Exotic::MappedArguments => Ok(self.heap.own_keys(o)),
            Exotic::Array => {
                let mut keys = self.heap.own_keys(o);
                let pos = keys
                    .iter()
                    .position(|k| !matches!(k, PropKey::Index(_)))
                    .unwrap_or(keys.len());
                keys.insert(pos, PropKey::Str(self.names.length));
                Ok(keys)
            }
            Exotic::String => {
                let Class::String(s) = &self.heap.obj(o).class else {
                    unreachable!()
                };
                let len = self.heap.str_len(*s) as u32;
                self.charge_list(len as usize, std::mem::size_of::<PropKey>())?;
                let rest = self.heap.own_keys(o);
                let mut keys: Vec<PropKey> = (0..len).map(PropKey::Index).collect();
                let mut after_indices = Vec::new();
                for k in rest {
                    match k {
                        PropKey::Index(i) if i < len => {}
                        PropKey::Index(_) => keys.push(k),
                        _ => after_indices.push(k),
                    }
                }
                keys.extend(after_indices);
                Ok(keys)
            }
            Exotic::TypedArray => typedarray::ta_own_keys(self, o),
            Exotic::Namespace => crate::modules::ns_own_keys(self, o),
            Exotic::Proxy => proxy::proxy_own_keys(self, o),
        }
    }

    // ---- abstract operations ------------------------------------------------------------

    pub fn create_data_property(&mut self, o: ObjId, key: PropKey, v: Value) -> JsResult<bool> {
        if self.exotic_of(o) == Exotic::None
            && !self.heap.has_own_property(o, key)
            && self.heap.obj(o).extensible
        {
            self.heap
                .add_own_property(o, key, Property::data(v, Attrs::ALL));
            return Ok(true);
        }
        let desc = PropertyDescriptor::data(v, Attrs::ALL);
        self.define_own_property(o, key, desc)
    }

    pub fn create_data_property_or_throw(
        &mut self,
        o: ObjId,
        key: PropKey,
        v: Value,
    ) -> JsResult<()> {
        if !self.create_data_property(o, key, v)? {
            let ks = self.display_key(key);
            return Err(self.throw_type(&format!("Cannot define property '{ks}' of object")));
        }
        Ok(())
    }

    pub fn define_property_or_throw(
        &mut self,
        o: ObjId,
        key: PropKey,
        desc: PropertyDescriptor,
    ) -> JsResult<()> {
        if !self.define_own_property(o, key, desc)? {
            let ks = self.display_key(key);
            return Err(self.throw_type(&format!("Cannot redefine property: {ks}")));
        }
        Ok(())
    }

    /// CreateMethodProperty-like helper used by builtins installers.
    pub fn define_method_property(&mut self, o: ObjId, key: PropKey, v: Value) -> JsResult<()> {
        let desc = PropertyDescriptor::data(v, Attrs::METHOD);
        self.define_property_or_throw(o, key, desc)
    }

    pub fn get_method(&mut self, v: Value, key: PropKey) -> JsResult<Option<Value>> {
        let f = self.get_value(v, key)?;
        if f.is_nullish() {
            return Ok(None);
        }
        if !self.is_callable(f) {
            let ks = self.display_key(key);
            return Err(self.throw_type(&format!("{ks} is not a function")));
        }
        Ok(Some(f))
    }

    pub fn invoke(&mut self, v: Value, key: PropKey, args: &[Value]) -> JsResult<Value> {
        let f = self.get_value(v, key)?;
        if !self.is_callable(f) {
            let ks = self.display_key(key);
            return Err(self.throw_type(&format!("{ks} is not a function")));
        }
        self.call_value(f, v, args)
    }

    pub fn invoke_str(&mut self, v: Value, name: &str, args: &[Value]) -> JsResult<Value> {
        let k = self.key(name);
        self.invoke(v, k, args)
    }

    /// SetIntegrityLevel(O, sealed | frozen).
    pub fn set_integrity_level(&mut self, o: ObjId, frozen: bool) -> JsResult<bool> {
        if !self.prevent_extensions(o)? {
            return Ok(false);
        }
        let keys = self.own_property_keys(o)?;
        for k in keys {
            if frozen {
                if let Some(cur) = self.own_property(o, k)? {
                    let desc = if cur.is_accessor() {
                        PropertyDescriptor {
                            configurable: Some(false),
                            ..Default::default()
                        }
                    } else {
                        PropertyDescriptor {
                            configurable: Some(false),
                            writable: Some(false),
                            ..Default::default()
                        }
                    };
                    self.define_property_or_throw(o, k, desc)?;
                }
            } else {
                let desc = PropertyDescriptor {
                    configurable: Some(false),
                    ..Default::default()
                };
                self.define_property_or_throw(o, k, desc)?;
            }
        }
        Ok(true)
    }

    pub fn test_integrity_level(&mut self, o: ObjId, frozen: bool) -> JsResult<bool> {
        if self.is_extensible(o)? {
            return Ok(false);
        }
        let keys = self.own_property_keys(o)?;
        for k in keys {
            if let Some(cur) = self.own_property(o, k)? {
                if cur.attrs.configurable() {
                    return Ok(false);
                }
                if frozen && !cur.is_accessor() && cur.attrs.writable() {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    /// CopyDataProperties(target, source, excluded).
    pub fn copy_data_properties(
        &mut self,
        target: ObjId,
        source: Value,
        excluded: &[PropKey],
    ) -> JsResult<()> {
        if source.is_nullish() {
            return Ok(());
        }
        let from = self.to_object(source)?;
        let keys = self.own_property_keys(from)?;
        for k in keys {
            if excluded.contains(&k) {
                continue;
            }
            if let Some(p) = self.own_property(from, k)? {
                if p.attrs.enumerable() {
                    let v = self.get_prop(from, k, Value::Object(from))?;
                    self.create_data_property_or_throw(target, k, v)?;
                }
            }
        }
        Ok(())
    }

    pub fn length_of_array_like(&mut self, o: ObjId) -> JsResult<f64> {
        if let Class::Array(a) = &self.heap.obj(o).class {
            return Ok(a.length as f64);
        }
        let v = self.get_prop(o, PropKey::Str(self.names.length), Value::Object(o))?;
        self.to_length(v)
    }

    /// A new array holding `items`, with %Array.prototype%.
    pub fn new_array(&mut self, items: Vec<Value>) -> ObjId {
        let proto = self.intrinsic(Intrinsic::ArrayPrototype);
        let len = items.len() as u32;
        let mut data = ObjectData::new(
            Some(proto),
            Class::Array(ArrayData {
                length: len,
                length_writable: true,
            }),
        );
        data.elements = Elements::Dense(items);
        self.alloc(data)
    }

    /// ArrayCreate(length, proto).
    pub fn array_create(&mut self, len: f64, proto: Option<ObjId>) -> JsResult<ObjId> {
        if len > u32::MAX as f64 || len < 0.0 || len.is_nan() {
            return Err(self.throw_range("Invalid array length"));
        }
        let proto = proto.unwrap_or_else(|| self.intrinsic(Intrinsic::ArrayPrototype));
        let data = ObjectData::new(
            Some(proto),
            Class::Array(ArrayData {
                length: len as u32,
                length_writable: true,
            }),
        );
        Ok(self.alloc(data))
    }

    /// CreateListFromArrayLike. Kept out of line: the interpreter's call
    /// path reaches it only for an array-like that is not a dense array,
    /// and inlining it there slowed every call.
    #[inline(never)]
    pub fn create_list_from_array_like(&mut self, v: Value) -> JsResult<Vec<Value>> {
        let Value::Object(o) = v else {
            return Err(self.throw_type("CreateListFromArrayLike called on non-object"));
        };
        if let Some(d) = self.heap.dense_values(o) {
            return Ok(d.to_vec());
        }
        let len = self.length_of_array_like(o)?;
        let n = self.check_list_length(len)?;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            self.poll()?;
            out.push(self.get_prop(o, PropKey::Index(i as u32), v)?);
        }
        Ok(out)
    }

    /// Room for a list of `len` values whose length a script chose (an
    /// array-like's `length`, a custom `exec` result's): a RangeError past
    /// [`MAX_LIST_LENGTH`], and the memory limit checked for the list
    /// before any of it is allocated (TASK-103.48, TASK-103.49).
    pub fn check_list_length(&mut self, len: f64) -> JsResult<usize> {
        if len > MAX_LIST_LENGTH as f64 {
            return Err(self.throw_range(&format!(
                "List of {len} values is too long (the maximum is {MAX_LIST_LENGTH})"
            )));
        }
        let n = len as usize;
        self.check_memory(n * std::mem::size_of::<Value>())?;
        Ok(n)
    }

    /// The property key for a non-negative integral number.
    pub fn number_key(&mut self, n: f64) -> PropKey {
        if n >= 0.0 && n < PropKey::MAX_INDEX as f64 && n.fract() == 0.0 {
            PropKey::Index(n as u32)
        } else {
            let s = agnijs_js_parser::number::to_js_string(n);
            self.key(&s)
        }
    }

    pub fn species_constructor(&mut self, o: ObjId, default: Intrinsic) -> JsResult<Value> {
        let c = self.get_prop(o, PropKey::Str(self.names.constructor), Value::Object(o))?;
        if c.is_undefined() {
            return Ok(Value::Object(self.intrinsic(default)));
        }
        let Value::Object(co) = c else {
            return Err(self.throw_type("object.constructor is not an object"));
        };
        let s = self.get_prop(co, PropKey::Sym(self.well_known.species), c)?;
        if s.is_nullish() {
            return Ok(Value::Object(self.intrinsic(default)));
        }
        if self.is_constructor(s) {
            return Ok(s);
        }
        Err(self.throw_type("object.constructor[Symbol.species] is not a constructor"))
    }

    /// OrdinaryHasInstance(C, O).
    pub fn ordinary_has_instance(&mut self, c: Value, o: Value) -> JsResult<bool> {
        if !self.is_callable(c) {
            return Ok(false);
        }
        let Value::Object(co) = c else {
            return Ok(false);
        };
        if let Class::Bound(b) = &self.heap.obj(co).class {
            let t = b.target;
            // InstanceofOperator looks up @@hasInstance on every link, so this
            // stays a recursion; a chain of bound functions with no prototype
            // reaches here without a call that checks the stack (TASK-103.47).
            self.check_stack()?;
            return self.instance_of(o, Value::Object(t));
        }
        let Value::Object(mut obj) = o else {
            return Ok(false);
        };
        let p = self.get_prop(co, PropKey::Str(self.names.prototype), c)?;
        let Value::Object(p) = p else {
            return Err(self.throw_type("Function has non-object prototype in instanceof check"));
        };
        loop {
            match self.get_prototype_of(obj)? {
                None => return Ok(false),
                Some(q) => {
                    if q == p {
                        return Ok(true);
                    }
                    obj = q;
                }
            }
        }
    }

    /// InstanceofOperator(V, target).
    pub fn instance_of(&mut self, v: Value, target: Value) -> JsResult<bool> {
        if !target.is_object() {
            return Err(self.throw_type("Right-hand side of 'instanceof' is not an object"));
        }
        let h = self.get_method(target, PropKey::Sym(self.well_known.has_instance))?;
        if let Some(h) = h {
            let r = self.call_value(h, target, &[v])?;
            return Ok(self.to_boolean(r));
        }
        if !self.is_callable(target) {
            return Err(self.throw_type("Right-hand side of 'instanceof' is not callable"));
        }
        self.ordinary_has_instance(target, v)
    }

    /// IsArray(argument).
    #[inline]
    pub fn is_array(&mut self, v: Value) -> JsResult<bool> {
        let Value::Object(o) = v else {
            return Ok(false);
        };
        match &self.heap.obj(o).class {
            Class::Array(_) => Ok(true),
            Class::Proxy(_) => self.is_array_through_proxies(o),
            _ => Ok(false),
        }
    }

    /// IsArray of a proxy: a loop through its targets, which can nest as
    /// deep as memory allows (TASK-103.47). Kept out of line so the common
    /// case above stays small.
    #[inline(never)]
    fn is_array_through_proxies(&mut self, mut o: ObjId) -> JsResult<bool> {
        loop {
            match &self.heap.obj(o).class {
                Class::Array(_) => return Ok(true),
                Class::Proxy(p) => match (p.target, p.handler) {
                    (Some(t), Some(_)) => o = t,
                    _ => {
                        return Err(self.throw_type(
                            "Cannot perform 'IsArray' on a proxy that has been revoked",
                        ))
                    }
                },
                _ => return Ok(false),
            }
        }
    }

    // ---- property descriptors as objects ---------------------------------------------------

    /// ToPropertyDescriptor.
    pub fn to_property_descriptor(&mut self, v: Value) -> JsResult<PropertyDescriptor> {
        let Value::Object(o) = v else {
            let d = self.describe_for_error(v);
            return Err(self.throw_type(&format!("Property description must be an object: {d}")));
        };
        let mut desc = PropertyDescriptor::default();
        macro_rules! field {
            ($name:ident, $set:expr) => {{
                let k = PropKey::Str(self.names.$name);
                if self.has_property(o, k)? {
                    let val = self.get_prop(o, k, v)?;
                    $set(self, &mut desc, val);
                }
            }};
        }
        field!(enumerable, |vm: &mut Vm,
                            d: &mut PropertyDescriptor,
                            val| d.enumerable =
            Some(vm.to_boolean(val)));
        field!(configurable, |vm: &mut Vm,
                              d: &mut PropertyDescriptor,
                              val| d
            .configurable =
            Some(vm.to_boolean(val)));
        field!(value, |_vm: &mut Vm, d: &mut PropertyDescriptor, val| d
            .value =
            Some(val));
        field!(writable, |vm: &mut Vm, d: &mut PropertyDescriptor, val| d
            .writable =
            Some(vm.to_boolean(val)));
        let gk = PropKey::Str(self.names.get);
        if self.has_property(o, gk)? {
            let g = self.get_prop(o, gk, v)?;
            if !g.is_undefined() && !self.is_callable(g) {
                let d = self.describe_for_error(g);
                return Err(self.throw_type(&format!("Getter must be a function: {d}")));
            }
            desc.get = Some(g);
        }
        let sk = PropKey::Str(self.names.set);
        if self.has_property(o, sk)? {
            let s = self.get_prop(o, sk, v)?;
            if !s.is_undefined() && !self.is_callable(s) {
                let d = self.describe_for_error(s);
                return Err(self.throw_type(&format!("Setter must be a function: {d}")));
            }
            desc.set = Some(s);
        }
        if (desc.get.is_some() || desc.set.is_some())
            && (desc.value.is_some() || desc.writable.is_some())
        {
            return Err(self.throw_type("Invalid property descriptor. Cannot both specify accessors and a value or writable attribute"));
        }
        Ok(desc)
    }

    /// FromPropertyDescriptor.
    pub fn from_property_descriptor(&mut self, desc: &PropertyDescriptor) -> Value {
        let o = self.new_object();
        if let Some(v) = desc.value {
            let k = PropKey::Str(self.names.value);
            self.heap
                .add_own_property(o, k, Property::data(v, Attrs::ALL));
        }
        if let Some(w) = desc.writable {
            let k = PropKey::Str(self.names.writable);
            self.heap
                .add_own_property(o, k, Property::data(Value::Bool(w), Attrs::ALL));
        }
        if let Some(g) = desc.get {
            let k = PropKey::Str(self.names.get);
            self.heap
                .add_own_property(o, k, Property::data(g, Attrs::ALL));
        }
        if let Some(s) = desc.set {
            let k = PropKey::Str(self.names.set);
            self.heap
                .add_own_property(o, k, Property::data(s, Attrs::ALL));
        }
        if let Some(e) = desc.enumerable {
            let k = PropKey::Str(self.names.enumerable);
            self.heap
                .add_own_property(o, k, Property::data(Value::Bool(e), Attrs::ALL));
        }
        if let Some(c) = desc.configurable {
            let k = PropKey::Str(self.names.configurable);
            self.heap
                .add_own_property(o, k, Property::data(Value::Bool(c), Attrs::ALL));
        }
        Value::Object(o)
    }

    // ---- iteration ----------------------------------------------------------------------------

    /// GetIterator(obj, sync | async) → (iterator, next method).
    pub fn get_iterator(&mut self, v: Value, is_async: bool) -> JsResult<(Value, Value)> {
        if is_async {
            let m = self.get_method(v, PropKey::Sym(self.well_known.async_iterator))?;
            match m {
                Some(m) => {
                    let it = self.call_value(m, v, &[])?;
                    if !it.is_object() {
                        return Err(self.throw_type(
                            "Result of the Symbol.asyncIterator method is not an object",
                        ));
                    }
                    let next = self.get_value(it, PropKey::Str(self.names.next))?;
                    return Ok((it, next));
                }
                None => {
                    let (it, next) = self.get_iterator(v, false)?;
                    let wrapped =
                        crate::builtins::iterator::create_async_from_sync_iterator(self, it, next);
                    let next = self.get_value(wrapped, PropKey::Str(self.names.next))?;
                    return Ok((wrapped, next));
                }
            }
        }
        let m = self.get_method(v, PropKey::Sym(self.well_known.iterator))?;
        let Some(m) = m else {
            let d = self.describe_for_error(v);
            return Err(self.throw_type(&format!("{d} is not iterable")));
        };
        let it = self.call_value(m, v, &[])?;
        if !it.is_object() {
            return Err(self.throw_type("Result of the Symbol.iterator method is not an object"));
        }
        let next = self.get_value(it, PropKey::Str(self.names.next))?;
        Ok((it, next))
    }

    /// IteratorStepValue: `Some(value)` or `None` when done.
    pub fn iterator_step_value(&mut self, iter: Value, next: Value) -> JsResult<Option<Value>> {
        let r = self.call_value(next, iter, &[])?;
        let Value::Object(ro) = r else {
            return Err(self.throw_type("Iterator result is not an object"));
        };
        let done = self.get_prop(ro, PropKey::Str(self.names.done), r)?;
        if self.to_boolean(done) {
            return Ok(None);
        }
        let v = self.get_prop(ro, PropKey::Str(self.names.value), r)?;
        Ok(Some(v))
    }

    /// IteratorClose with a normal completion.
    pub fn iterator_close(&mut self, iter: Value) -> JsResult<()> {
        let ret = self.get_method(iter, PropKey::Str(self.names.return_))?;
        if let Some(ret) = ret {
            let r = self.call_value(ret, iter, &[])?;
            if !r.is_object() {
                return Err(self.throw_type("Iterator return() result is not an object"));
            }
        }
        Ok(())
    }

    /// IteratorClose with a throw completion: errors from `return` are
    /// swallowed in favour of the original one.
    pub fn iterator_close_swallow(&mut self, iter: Value) {
        if let Ok(Some(ret)) = self.get_method(iter, PropKey::Str(self.names.return_)) {
            let _ = self.call_value(ret, iter, &[]);
        }
    }

    /// IteratorToList(GetIterator(v)).
    pub fn iterable_to_list(&mut self, v: Value) -> JsResult<Vec<Value>> {
        // Fast path: arrays with the pristine iterator.
        if let Value::Object(o) = v {
            if self.array_iteration_is_pristine(o) {
                if let Some(d) = self.heap.dense_values(o) {
                    return Ok(d.to_vec());
                }
            }
        }
        let (it, next) = self.get_iterator(v, false)?;
        let mut out = Vec::new();
        while let Some(x) = self.iterator_step_value(it, next)? {
            // A native iterator can yield without end; the list counts
            // towards the limit as it grows (TASK-103.53).
            self.heap.charge(std::mem::size_of::<Value>());
            self.poll()?;
            out.push(x);
        }
        Ok(out)
    }

    /// Whether iterating the array `o` would observe only the built-in
    /// behaviour (own @@iterator absent, %Array.prototype%[@@iterator] and
    /// %ArrayIteratorPrototype%.next unmodified).
    pub fn array_iteration_is_pristine(&mut self, o: ObjId) -> bool {
        let Class::Array(_) = &self.heap.obj(o).class else {
            return false;
        };
        let ap = self.intrinsic(Intrinsic::ArrayPrototype);
        if self.heap.obj(o).proto() != Some(ap) {
            return false;
        }
        let it_key = PropKey::Sym(self.well_known.iterator);
        if self.heap.has_own_property(o, it_key) {
            return false;
        }
        let values = self.intrinsic(Intrinsic::ArrayPrototypeValues);
        match self.heap.get_own_property(ap, it_key) {
            Some(p) if p.value() == Some(Value::Object(values)) => {}
            _ => return false,
        }
        let aip = self.intrinsic(Intrinsic::ArrayIteratorPrototype);
        let nk = PropKey::Str(self.names.next);
        match self.heap.get_own_property(aip, nk) {
            Some(p) => match p.value() {
                Some(Value::Object(f)) => match &self.heap.obj(f).class {
                    Class::Native(n) => {
                        n.flag.is_none()
                            && matches!(n.func, crate::object::NativeKind::Fn(_))
                            && self.is_pristine_array_iterator_next(f)
                    }
                    _ => false,
                },
                _ => false,
            },
            None => false,
        }
    }

    fn is_pristine_array_iterator_next(&self, f: ObjId) -> bool {
        // The installer stores the original next function in the realm's
        // iterator prototype slot; compare by pointer.
        match &self.heap.obj(f).class {
            Class::Native(n) => match n.func {
                crate::object::NativeKind::Fn(p) => {
                    p as *const () as usize
                        == crate::builtins::iterator::array_iterator_next as *const () as usize
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// for-in: EnumerateObjectProperties.
    pub fn for_in_prepare(&mut self, v: Value) -> JsResult<ObjId> {
        let proto = self.intrinsic(Intrinsic::ForInIteratorPrototype);
        let state = match v {
            Value::Undefined | Value::Null => ForInState {
                object: None,
                keys: Vec::new(),
                index: 0,
                visited: Vec::new(),
            },
            _ => {
                let o = self.to_object(v)?;
                let keys = self.own_property_keys(o)?;
                ForInState {
                    object: Some(o),
                    keys,
                    index: 0,
                    visited: Vec::new(),
                }
            }
        };
        Ok(self.alloc(ObjectData::new(Some(proto), Class::ForIn(Box::new(state)))))
    }

    /// The next enumerable string key, or `None` when exhausted.
    pub fn for_in_next(&mut self, it: ObjId) -> JsResult<Option<Value>> {
        loop {
            let (obj, key) = {
                let Class::ForIn(s) = &mut self.heap.obj_mut(it).class else {
                    unreachable!()
                };
                let Some(obj) = s.object else { return Ok(None) };
                if s.index >= s.keys.len() {
                    // Move to the prototype.
                    (obj, None)
                } else {
                    let k = s.keys[s.index];
                    s.index += 1;
                    (obj, Some(k))
                }
            };
            match key {
                None => {
                    let proto = self.get_prototype_of(obj)?;
                    let Class::ForIn(s) = &mut self.heap.obj_mut(it).class else {
                        unreachable!()
                    };
                    match proto {
                        None => {
                            s.object = None;
                            return Ok(None);
                        }
                        Some(p) => {
                            s.object = Some(p);
                            s.index = 0;
                            s.keys = Vec::new();
                            let keys = self.own_property_keys(p)?;
                            let Class::ForIn(s) = &mut self.heap.obj_mut(it).class else {
                                unreachable!()
                            };
                            s.keys = keys;
                        }
                    }
                }
                Some(PropKey::Sym(_)) => continue,
                Some(k) => {
                    let visited = {
                        let Class::ForIn(s) = &self.heap.obj(it).class else {
                            unreachable!()
                        };
                        s.visited.contains(&k)
                    };
                    if visited {
                        continue;
                    }
                    let Some(p) = self.own_property(obj, k)? else {
                        continue;
                    };
                    {
                        let Class::ForIn(s) = &mut self.heap.obj_mut(it).class else {
                            unreachable!()
                        };
                        s.visited.push(k);
                    }
                    if !p.attrs.enumerable() {
                        continue;
                    }
                    return Ok(Some(self.key_to_value(k)));
                }
            }
        }
    }

    /// EnumerableOwnProperties(O, kind) where kind is keys (0), values (1), entries (2).
    /// EnumerableOwnProperties(O, key) as property keys rather than
    /// strings: the same [[OwnPropertyKeys]] and [[GetOwnProperty]] calls,
    /// without materialising a string for each index key.
    pub fn enumerable_own_keys(&mut self, o: ObjId) -> JsResult<Vec<PropKey>> {
        let mut keys = self.own_property_keys(o)?;
        let mut kept = 0;
        for i in 0..keys.len() {
            let k = keys[i];
            if matches!(k, PropKey::Sym(_)) {
                continue;
            }
            let Some(p) = self.own_property(o, k)? else {
                continue;
            };
            if p.attrs.enumerable() {
                keys[kept] = k;
                kept += 1;
            }
        }
        keys.truncate(kept);
        Ok(keys)
    }

    pub fn enumerable_own_properties(&mut self, o: ObjId, kind: u8) -> JsResult<Vec<Value>> {
        let keys = self.own_property_keys(o)?;
        // new_array charges the list when it takes it; check it first.
        self.check_memory(keys.len() * std::mem::size_of::<Value>())?;
        let mut out = Vec::with_capacity(keys.len());
        for k in keys {
            self.poll()?;
            if matches!(k, PropKey::Sym(_)) {
                continue;
            }
            let Some(p) = self.own_property(o, k)? else {
                continue;
            };
            if !p.attrs.enumerable() {
                continue;
            }
            let kv = self.key_to_value(k);
            match kind {
                0 => out.push(kv),
                1 => out.push(self.get_prop(o, k, Value::Object(o))?),
                _ => {
                    let v = self.get_prop(o, k, Value::Object(o))?;
                    let pair = self.new_array(vec![kv, v]);
                    out.push(Value::Object(pair));
                }
            }
        }
        Ok(out)
    }
}
