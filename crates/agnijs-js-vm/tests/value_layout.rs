//! The defined layout of `Value` (ADR 0019 J5, TASK-101.3): compiled code
//! reads and writes register-file values by these offsets and tags, so they
//! are a contract, checked here byte by byte.

use agnijs_js_vm::value::{ObjId, PropSlot, StrId, Value};

/// The tag byte of a value. `Value` is `#[repr(C, u8)]`, so byte 0 is always
/// the (initialised) discriminant.
fn tag(v: Value) -> u8 {
    // SAFETY: a `#[repr(C, u8)]` enum starts with its `u8` tag.
    unsafe { *(&v as *const Value as *const u8) }
}

/// The eight payload bytes of a value whose payload is eight bytes wide.
fn payload_u64(v: Value) -> u64 {
    // SAFETY: the payload union starts at `Value::PAYLOAD_OFFSET`; the
    // caller only passes `Number` values, whose eight payload bytes are set.
    unsafe {
        std::ptr::read_unaligned(
            (&v as *const Value as *const u8).add(Value::PAYLOAD_OFFSET) as *const u64,
        )
    }
}

fn payload_u32(v: Value) -> u32 {
    // SAFETY: as above; id payloads are four initialised bytes.
    unsafe {
        std::ptr::read_unaligned(
            (&v as *const Value as *const u8).add(Value::PAYLOAD_OFFSET) as *const u32,
        )
    }
}

#[test]
fn value_is_sixteen_bytes_with_the_tag_first_and_the_payload_at_eight() {
    assert_eq!(std::mem::size_of::<Value>(), 16);
    assert_eq!(std::mem::align_of::<Value>(), 8);
    assert_eq!(Value::TAG_OFFSET, 0);
    assert_eq!(Value::PAYLOAD_OFFSET, 8);
    // The layout keeps the niches the rest of the engine packs into.
    assert_eq!(std::mem::size_of::<Option<Value>>(), 16);
    assert_eq!(std::mem::size_of::<PropSlot>(), 16);
}

#[test]
fn tags_are_the_declared_constants() {
    assert_eq!(tag(Value::Undefined), 0);
    assert_eq!(tag(Value::Null), 1);
    assert_eq!(tag(Value::Bool(true)), 2);
    assert_eq!(tag(Value::Number(1.5)), 3);
    assert_eq!(tag(Value::String(StrId(7))), 4);
    assert_eq!(tag(Value::Object(ObjId(9))), 7);
    assert_eq!(tag(Value::Empty), 8);
    assert_eq!(Value::TAG_UNDEFINED, 0);
    assert_eq!(Value::TAG_NULL, 1);
    assert_eq!(Value::TAG_BOOL, 2);
    assert_eq!(Value::TAG_NUMBER, 3);
    assert_eq!(Value::TAG_STRING, 4);
    assert_eq!(Value::TAG_SYMBOL, 5);
    assert_eq!(Value::TAG_BIGINT, 6);
    assert_eq!(Value::TAG_OBJECT, 7);
    assert_eq!(Value::TAG_EMPTY, 8);
}

#[test]
fn payloads_sit_at_the_payload_offset() {
    assert_eq!(payload_u64(Value::Number(-2.25)), (-2.25f64).to_bits());
    assert_eq!(payload_u32(Value::Object(ObjId(0x1234_5678))), 0x1234_5678);
    assert_eq!(payload_u32(Value::String(StrId(42))), 42);
    // SAFETY: a `Bool` payload is one initialised byte at the offset.
    let b = unsafe { *(&Value::Bool(true) as *const Value as *const u8).add(8) };
    assert_eq!(b, 1);
}
