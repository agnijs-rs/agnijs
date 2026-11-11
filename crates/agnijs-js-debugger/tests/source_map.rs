//! Source map decoding against a map produced by an independent encoder
//! (`source-map-js`'s `SourceMapGenerator`); the expected positions are what
//! `source-map-js`'s consumer returns for the same queries (its 1-based lines
//! converted to 0-based).

use agnijs_js_debugger::{source_mapping_url, OriginalPosition, SourceMap};

const MAP: &str = r#"{"version":3,"sources":["app.ts","lib/other.ts"],"names":["fail","msg","Error","origin"],"mappings":"AACA,SAASA;EACP,MAAMC;EACN,UAAUC;AACZ;AACA,MAAMC;AACN,KC6FG","file":"app.js","sourceRoot":"src","sourcesContent":["// original",null]}"#;

fn pos(source: &str, line: u32, column: u32, name: Option<&str>) -> Option<OriginalPosition> {
    Some(OriginalPosition {
        source: source.into(),
        line,
        column,
        name: name.map(Into::into),
    })
}

#[test]
fn generated_positions_map_to_the_original_source() {
    let map = SourceMap::parse(MAP).unwrap();
    assert_eq!(map.lookup(0, 0), pos("src/app.ts", 1, 0, None));
    assert_eq!(map.lookup(0, 12), pos("src/app.ts", 1, 9, Some("fail")));
    assert_eq!(map.lookup(1, 10), pos("src/app.ts", 2, 8, Some("msg")));
    assert_eq!(map.lookup(2, 2), pos("src/app.ts", 3, 2, None));
    assert_eq!(map.lookup(2, 20), pos("src/app.ts", 3, 12, Some("Error")));
    assert_eq!(map.lookup(3, 0), pos("src/app.ts", 4, 0, None));
    assert_eq!(map.lookup(4, 7), pos("src/app.ts", 5, 6, Some("origin")));
    assert_eq!(map.lookup(5, 5), pos("src/lib/other.ts", 99, 3, None));
    assert_eq!(map.lookup(5, 9), pos("src/lib/other.ts", 99, 3, None));
    assert_eq!(map.sources_content[0].as_deref(), Some("// original"));
    assert_eq!(map.file.as_deref(), Some("app.js"));
}

#[test]
fn positions_before_the_first_segment_or_past_the_last_line_are_unmapped() {
    let map = SourceMap::parse(MAP).unwrap();
    assert_eq!(map.lookup(1, 0), None);
    assert_eq!(map.lookup(40, 0), None);
}

#[test]
fn inline_data_url_maps_decode() {
    // {"version":3,"sources":["a.js"],"names":[],"mappings":"AAAA,IAAI"},
    // base64-encoded by Node's Buffer.
    let url = "data:application/json;charset=utf-8;base64,eyJ2ZXJzaW9uIjozLCJzb3VyY2VzIjpbImEuanMiXSwibmFtZXMiOltdLCJtYXBwaW5ncyI6IkFBQUEsSUFBSSJ9";
    let map = SourceMap::from_data_url(url).unwrap().unwrap();
    assert_eq!(map.lookup(0, 5), pos("a.js", 0, 4, None));
    assert!(SourceMap::from_data_url("app.js.map").is_none());
}

#[test]
fn malformed_maps_are_rejected() {
    assert!(SourceMap::parse(r#"{"version":2,"sources":[],"names":[],"mappings":""}"#).is_err());
    assert!(SourceMap::parse(r#"{"version":3,"sources":[],"names":[],"mappings":"AAAA"}"#).is_err());
    assert!(SourceMap::parse(r#"{"version":3,"sources":["a"],"names":[],"mappings":"AAA"}"#).is_err());
}

#[test]
fn hostile_maps_are_rejected_without_panicking() {
    let map = |mappings: &str| {
        format!(r#"{{"version":3,"sources":["a"],"names":["n"],"mappings":"{mappings}"}}"#)
    };
    // Generated columns that overflow when summed.
    let big = "+/////D"; // a VLQ near the 2^31 bound
    let many = vec![big; 4096].join(",");
    assert!(SourceMap::parse(&map(&many)).is_err());
    // A single value past the 32-bit range.
    assert!(SourceMap::parse(&map("gggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggB")).is_err());
    assert!(SourceMap::parse(&map("AAAA,ggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggB")).is_err());
    // Percent escapes cut by a multi-byte character.
    assert!(SourceMap::from_data_url("data:application/json,%\u{e9}\u{e9}").unwrap().is_err());
    assert!(SourceMap::from_data_url("data:application/json,%e\u{e9}").unwrap().is_err());
    // Deep JSON nesting.
    let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
    assert!(SourceMap::parse(&deep).is_err());
}

#[test]
fn oversized_maps_are_rejected() {
    let mappings = ";".repeat(agnijs_js_debugger::source_map::MAX_LINES + 1);
    let text = format!(r#"{{"version":3,"sources":["a"],"names":[],"mappings":"{mappings}"}}"#);
    assert!(SourceMap::parse(&text).is_err());
    let segs = vec!["AAAA"; agnijs_js_debugger::source_map::MAX_SEGMENTS + 1].join(",");
    let text = format!(r#"{{"version":3,"sources":["a"],"names":[],"mappings":"{segs}"}}"#);
    assert!(SourceMap::parse(&text).is_err());
}

/// A bounded, deterministic fuzz: mutations of a valid map and random
/// text must decode or fail, never panic, and lookups on what decodes must
/// not panic either.
#[test]
fn fuzzed_maps_never_panic() {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=,;\"{}[]:-_%\\ \n";
    for round in 0..3000 {
        let mut bytes: Vec<u8> = if round % 3 == 0 {
            (0..(next() % 200)).map(|_| ALPHABET[(next() % ALPHABET.len() as u64) as usize]).collect()
        } else {
            MAP.as_bytes().to_vec()
        };
        for _ in 0..(1 + next() % 8) {
            if bytes.is_empty() {
                break;
            }
            let i = (next() % bytes.len() as u64) as usize;
            match next() % 3 {
                0 => bytes[i] = ALPHABET[(next() % ALPHABET.len() as u64) as usize],
                1 => {
                    bytes.remove(i);
                }
                _ => bytes.insert(i, ALPHABET[(next() % ALPHABET.len() as u64) as usize]),
            }
        }
        let text = String::from_utf8_lossy(&bytes);
        if let Ok(m) = SourceMap::parse(&text) {
            for _ in 0..16 {
                let _ = m.lookup((next() % 12) as u32, (next() % 64) as u32);
            }
        }
        let _ = SourceMap::from_data_url(&format!("data:application/json,{text}"));
        let _ = SourceMap::from_data_url(&format!("data:application/json;base64,{text}"));
    }
}

#[test]
fn the_trailing_source_mapping_url_comment_is_found() {
    assert_eq!(
        source_mapping_url("let a = 1;\n//# sourceMappingURL=app.js.map\n").as_deref(),
        Some("app.js.map")
    );
    assert_eq!(source_mapping_url("//# sourceMappingURL=x.map\nlet a = 1;\n"), None);
    assert_eq!(source_mapping_url("let a = 1;"), None);
}
