//! Source Map revision 3 decoding (the format of
//! <https://tc39.es/source-map/>): enough to map a generated position back
//! to its original source, for source-mapped exception reports.

use serde_json::Value as Json;

/// A decoded source map.
#[derive(Clone, Debug)]
pub struct SourceMap {
    pub file: Option<String>,
    /// Source URLs with `sourceRoot` applied.
    pub sources: Vec<String>,
    pub sources_content: Vec<Option<String>>,
    pub names: Vec<String>,
    /// Per generated line, segments sorted by generated column.
    lines: Vec<Vec<Segment>>,
}

#[derive(Clone, Copy, Debug)]
struct Segment {
    column: u32,
    original: Option<(u32, u32, u32, Option<u32>)>,
}

/// Where a generated position came from. Lines and columns are 0-based.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OriginalPosition {
    pub source: String,
    pub line: u32,
    pub column: u32,
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceMapError(pub String);

impl std::fmt::Display for SourceMapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid source map: {}", self.0)
    }
}

fn err<T>(m: impl Into<String>) -> Result<T, SourceMapError> {
    Err(SourceMapError(m.into()))
}

impl SourceMap {
    /// Decode the JSON text of a map.
    pub fn parse(json: &str) -> Result<SourceMap, SourceMapError> {
        let v: Json = serde_json::from_str(json).map_err(|e| SourceMapError(e.to_string()))?;
        if v.get("version").and_then(Json::as_u64) != Some(3) {
            return err("version is not 3");
        }
        if v.get("sections").is_some() {
            return err("indexed source maps are not supported");
        }
        let strings = |key: &str| -> Vec<Option<String>> {
            v.get(key)
                .and_then(Json::as_array)
                .map(|a| a.iter().map(|s| s.as_str().map(str::to_string)).collect())
                .unwrap_or_default()
        };
        let root = v
            .get("sourceRoot")
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_string();
        let sources = strings("sources")
            .into_iter()
            .map(|s| {
                let s = s.unwrap_or_default();
                if root.is_empty() || s.contains("://") || s.starts_with('/') {
                    s
                } else if root.ends_with('/') {
                    format!("{root}{s}")
                } else {
                    format!("{root}/{s}")
                }
            })
            .collect::<Vec<_>>();
        let names = strings("names")
            .into_iter()
            .map(Option::unwrap_or_default)
            .collect::<Vec<_>>();
        let Some(mappings) = v.get("mappings").and_then(Json::as_str) else {
            return err("missing mappings");
        };
        let lines = decode_mappings(mappings, sources.len(), names.len())?;
        Ok(SourceMap {
            file: v.get("file").and_then(Json::as_str).map(str::to_string),
            sources_content: strings("sourcesContent"),
            sources,
            names,
            lines,
        })
    }

    /// Decode a `data:application/json[;charset=...];base64,` URL (or a
    /// URL-encoded `data:application/json,` one). `None` when `url` is not a
    /// JSON data URL.
    pub fn from_data_url(url: &str) -> Option<Result<SourceMap, SourceMapError>> {
        let rest = url.strip_prefix("data:")?;
        let (meta, data) = rest.split_once(',')?;
        let mut parts = meta.split(';');
        if parts.next()? != "application/json" {
            return None;
        }
        let base64 = meta.split(';').any(|p| p == "base64");
        let text = if base64 {
            match base64_decode(data) {
                Some(bytes) => match String::from_utf8(bytes) {
                    Ok(s) => s,
                    Err(_) => return Some(err("data URL is not UTF-8")),
                },
                None => return Some(err("bad base64 in data URL")),
            }
        } else {
            percent_decode(data)
        };
        Some(SourceMap::parse(&text))
    }

    /// The original position of generated `(line, column)`: the mapping
    /// segment at or before the column on that line.
    pub fn lookup(&self, line: u32, column: u32) -> Option<OriginalPosition> {
        let segs = self.lines.get(line as usize)?;
        let i = segs.partition_point(|s| s.column <= column);
        let seg = segs.get(i.checked_sub(1)?)?;
        let (src, oline, ocol, name) = seg.original?;
        Some(OriginalPosition {
            source: self.sources.get(src as usize)?.clone(),
            line: oline,
            column: ocol,
            name: name.and_then(|n| self.names.get(n as usize).cloned()),
        })
    }
}

fn base64_value(c: u8) -> Option<u32> {
    Some(match c {
        b'A'..=b'Z' => (c - b'A') as u32,
        b'a'..=b'z' => (c - b'a') as u32 + 26,
        b'0'..=b'9' => (c - b'0') as u32 + 52,
        b'+' | b'-' => 62,
        b'/' | b'_' => 63,
        _ => return None,
    })
}

/// Standard (or URL-safe) base64, padding optional.
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in text.bytes() {
        if c == b'=' {
            break;
        }
        if c.is_ascii_whitespace() {
            continue;
        }
        acc = (acc << 6) | base64_value(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn decode_mappings(
    text: &str,
    source_count: usize,
    name_count: usize,
) -> Result<Vec<Vec<Segment>>, SourceMapError> {
    let mut lines = Vec::new();
    // Source, original line, original column and name are relative to the
    // previous segment across lines; the generated column resets per line.
    let (mut src, mut oline, mut ocol, mut name) = (0i64, 0i64, 0i64, 0i64);
    for line in text.split(';') {
        let mut segs = Vec::new();
        let mut col = 0i64;
        for seg in line.split(',') {
            if seg.is_empty() {
                continue;
            }
            let fields = decode_vlq(seg)?;
            match fields.len() {
                1 | 4 | 5 => {}
                n => return err(format!("segment with {n} fields")),
            }
            col += fields[0];
            if col < 0 {
                return err("negative generated column");
            }
            let original = if fields.len() >= 4 {
                src += fields[1];
                oline += fields[2];
                ocol += fields[3];
                if src < 0 || src as usize >= source_count || oline < 0 || ocol < 0 {
                    return err("segment outside the sources");
                }
                let n = if fields.len() == 5 {
                    name += fields[4];
                    if name < 0 || name as usize >= name_count {
                        return err("name index out of range");
                    }
                    Some(name as u32)
                } else {
                    None
                };
                Some((src as u32, oline as u32, ocol as u32, n))
            } else {
                None
            };
            segs.push(Segment {
                column: col as u32,
                original,
            });
        }
        segs.sort_by_key(|s| s.column);
        lines.push(segs);
    }
    Ok(lines)
}

fn decode_vlq(seg: &str) -> Result<Vec<i64>, SourceMapError> {
    let mut out = Vec::new();
    let mut value = 0i64;
    let mut shift = 0;
    for c in seg.bytes() {
        let Some(d) = base64_value(c).filter(|_| c != b'-' && c != b'_') else {
            return err(format!("bad VLQ digit {:?}", c as char));
        };
        let d = d as i64;
        if shift > 60 {
            return err("VLQ value too large");
        }
        value |= (d & 31) << shift;
        if d & 32 != 0 {
            shift += 5;
        } else {
            let v = value >> 1;
            out.push(if value & 1 == 1 { -v } else { v });
            value = 0;
            shift = 0;
        }
    }
    if shift != 0 {
        return err("unterminated VLQ value");
    }
    Ok(out)
}
