//! Source map decoding against a map produced by an independent encoder
//! (`source-map-js`'s `SourceMapGenerator`); the expected positions are what
//! `source-map-js`'s consumer returns for the same queries (its 1-based lines
//! converted to 0-based).

use gable_js_debugger::{source_mapping_url, OriginalPosition, SourceMap};

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
fn the_trailing_source_mapping_url_comment_is_found() {
    assert_eq!(
        source_mapping_url("let a = 1;\n//# sourceMappingURL=app.js.map\n").as_deref(),
        Some("app.js.map")
    );
    assert_eq!(source_mapping_url("//# sourceMappingURL=x.map\nlet a = 1;\n"), None);
    assert_eq!(source_mapping_url("let a = 1;"), None);
}
