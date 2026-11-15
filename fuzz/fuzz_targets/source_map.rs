#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| agnijs_fuzz::source_map(data));
