//! serde interop: Rust data to JavaScript values and back.
//!
//! The mapping is JSON's, with numbers as JavaScript numbers: structs and
//! maps become plain objects, sequences and tuples become arrays, `Option`
//! `None` becomes `null`, and integers outside +-2^53 lose precision.
//! `undefined` reads back as `null`; non-finite numbers read back as `null`;
//! functions, symbols, BigInts and cyclic structures are conversion errors.

use crate::error::{Error, Result};
use agnijs_engine_api::{Engine, Handle, PropertyKey, RealmId, TypeOf, Value};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{Map, Number, Value as Json};

const MAX_DEPTH: usize = 128;

/// Convert a Rust value to a JavaScript value in `realm`.
pub fn to_value<T: Serialize + ?Sized>(
    engine: &mut dyn Engine,
    realm: RealmId,
    value: &T,
) -> Result<Value> {
    let json = serde_json::to_value(value).map_err(|e| Error::Convert(e.to_string()))?;
    json_to_js(engine, realm, &json)
}

/// Convert a JavaScript value to a Rust value.
pub fn from_value<T: DeserializeOwned>(
    engine: &mut dyn Engine,
    realm: RealmId,
    value: &Value,
) -> Result<T> {
    let json = js_to_json(engine, realm, value)?;
    serde_json::from_value(json).map_err(|e| Error::Convert(e.to_string()))
}

pub(crate) fn json_to_js(engine: &mut dyn Engine, realm: RealmId, json: &Json) -> Result<Value> {
    Ok(match json {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => Value::Number(n.as_f64().unwrap_or(f64::NAN)),
        Json::String(s) => Value::string(s),
        Json::Array(items) => {
            let mut vals = Vec::with_capacity(items.len());
            for i in items {
                vals.push(json_to_js(engine, realm, i)?);
            }
            engine.new_array(realm, &vals)?
        }
        Json::Object(map) => {
            let obj = engine.new_object(realm)?;
            for (k, v) in map {
                let v = json_to_js(engine, realm, v)?;
                engine.set(realm, &obj, &PropertyKey::from(k.as_str()), &v)?;
            }
            obj
        }
    })
}

pub(crate) fn js_to_json(engine: &mut dyn Engine, realm: RealmId, value: &Value) -> Result<Json> {
    walk(engine, realm, value, &mut Vec::new())
}

fn walk(
    engine: &mut dyn Engine,
    realm: RealmId,
    value: &Value,
    path: &mut Vec<Handle>,
) -> Result<Json> {
    Ok(match value {
        Value::Undefined | Value::Null => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Number(n) => number(*n),
        Value::String(s) => Json::String(s.to_string_lossy()),
        Value::BigInt(_) => return Err(Error::Convert("a BigInt has no data mapping".into())),
        Value::Symbol(_) => return Err(Error::Convert("a symbol has no data mapping".into())),
        Value::Object(h) => {
            if engine.type_of(value) == TypeOf::Function {
                return Err(Error::Convert("a function has no data mapping".into()));
            }
            if path.len() >= MAX_DEPTH || path.iter().any(|p| p == h) {
                return Err(Error::Convert("cyclic or too deeply nested value".into()));
            }
            path.push(h.clone());
            let out = if engine.is_array(value) {
                let len = engine.get(realm, value, &"length".into())?;
                let len = len.as_number().unwrap_or(0.0) as u32;
                let mut items = Vec::with_capacity(len as usize);
                for i in 0..len {
                    let v = engine.get(realm, value, &PropertyKey::from(i))?;
                    items.push(walk(engine, realm, &v, path)?);
                }
                Json::Array(items)
            } else {
                let mut map = Map::new();
                for key in engine.own_keys(realm, value)? {
                    let PropertyKey::String(name) = &key else {
                        continue;
                    };
                    let v = engine.get(realm, value, &key)?;
                    map.insert(name.to_string_lossy(), walk(engine, realm, &v, path)?);
                }
                Json::Object(map)
            };
            path.pop();
            out
        }
    })
}

fn number(n: f64) -> Json {
    if n.is_finite() && n.fract() == 0.0 && n.abs() <= 9_007_199_254_740_992.0 {
        Json::Number(Number::from(n as i64))
    } else {
        Number::from_f64(n).map(Json::Number).unwrap_or(Json::Null)
    }
}

/// Decode a call's argument list as `A`: normally a tuple from the array
/// of arguments, and `()` for none.
pub(crate) fn decode_args<A: DeserializeOwned>(args: Vec<Json>) -> std::result::Result<A, String> {
    let empty = args.is_empty();
    match serde_json::from_value(Json::Array(args)) {
        Ok(a) => Ok(a),
        Err(e) if !empty => Err(e.to_string()),
        Err(e) => serde_json::from_value(Json::Null).map_err(|_| e.to_string()),
    }
}
