#!/usr/bin/env node
// Generates crates/agnijs-js-regex/src/unicode_tables.rs from the Unicode
// Character Database (TASK-100.31).
//
// The regex engine carries its own tables, pinned to the Unicode version
// test262 generates its property-escape tests from, rather than the ICU4X
// data the Intl crate uses (which can be a version ahead).
//
//   node tools/gen-regex-unicode.mjs [--ucd <dir>] [--version 17.0.0]
//
// Missing UCD files are downloaded into the cache directory (default
// target/ucd-<version>), so a rerun is offline.

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);
const opt = (name, dflt) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : dflt;
};
const version = opt('--version', '17.0.0');
const cache = path.resolve(opt('--ucd', path.join(root, 'target', `ucd-${version}`)));
const out = path.join(root, 'crates', 'agnijs-js-regex', 'src', 'unicode_tables.rs');

const FILES = {
  UnicodeData: 'ucd/UnicodeData.txt',
  DerivedCoreProperties: 'ucd/DerivedCoreProperties.txt',
  PropList: 'ucd/PropList.txt',
  DerivedBinaryProperties: 'ucd/extracted/DerivedBinaryProperties.txt',
  DerivedNormalizationProps: 'ucd/DerivedNormalizationProps.txt',
  EmojiData: 'ucd/emoji/emoji-data.txt',
  Scripts: 'ucd/Scripts.txt',
  ScriptExtensions: 'ucd/ScriptExtensions.txt',
  PropertyValueAliases: 'ucd/PropertyValueAliases.txt',
  CaseFolding: 'ucd/CaseFolding.txt',
  SpecialCasing: 'ucd/SpecialCasing.txt',
  EmojiSequences: 'emoji/emoji-sequences.txt',
  EmojiZwjSequences: 'emoji/emoji-zwj-sequences.txt',
};

async function load(key) {
  const rel = FILES[key];
  const file = path.join(cache, rel);
  if (!fs.existsSync(file)) {
    const url = `https://www.unicode.org/Public/${version}/${rel}`;
    process.stderr.write(`fetching ${url}\n`);
    const res = await fetch(url);
    if (!res.ok) throw new Error(`${url}: ${res.status}`);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, await res.text());
  }
  return fs.readFileSync(file, 'utf8');
}

/** Data lines split on `;`, comments and blanks removed. */
function rows(text) {
  const out = [];
  for (const line of text.split('\n')) {
    const body = line.split('#')[0].trim();
    if (body) out.push(body.split(';').map((f) => f.trim()));
  }
  return out;
}

function range(field) {
  const [a, b] = field.split('..');
  return [parseInt(a, 16), parseInt(b ?? a, 16)];
}

/** A set of code points as sorted, merged inclusive ranges. */
class Ranges {
  constructor() {
    this.list = [];
  }
  add(lo, hi) {
    this.list.push([lo, hi]);
    return this;
  }
  normalized() {
    const l = this.list.slice().sort((x, y) => x[0] - y[0]);
    const out = [];
    for (const [lo, hi] of l) {
      const last = out[out.length - 1];
      if (last && lo <= last[1] + 1) last[1] = Math.max(last[1], hi);
      else out.push([lo, hi]);
    }
    return out;
  }
}

function propertyFile(text, wanted) {
  const sets = new Map();
  for (const [r, name] of rows(text)) {
    if (wanted && !wanted.has(name)) continue;
    if (!sets.has(name)) sets.set(name, new Ranges());
    const [lo, hi] = range(r);
    sets.get(name).add(lo, hi);
  }
  return sets;
}

// ES2025 table 69 (binary properties) with their aliases; Any, ASCII and
// Assigned are derived.
const BINARY = [
  ['ASCII_Hex_Digit', 'AHex'], ['Alphabetic', 'Alpha'], ['Bidi_Control', 'Bidi_C'],
  ['Bidi_Mirrored', 'Bidi_M'], ['Case_Ignorable', 'CI'], ['Cased'],
  ['Changes_When_Casefolded', 'CWCF'], ['Changes_When_Casemapped', 'CWCM'],
  ['Changes_When_Lowercased', 'CWL'], ['Changes_When_NFKC_Casefolded', 'CWKCF'],
  ['Changes_When_Titlecased', 'CWT'], ['Changes_When_Uppercased', 'CWU'], ['Dash'],
  ['Default_Ignorable_Code_Point', 'DI'], ['Deprecated', 'Dep'], ['Diacritic', 'Dia'], ['Emoji'],
  ['Emoji_Component', 'EComp'], ['Emoji_Modifier', 'EMod'], ['Emoji_Modifier_Base', 'EBase'],
  ['Emoji_Presentation', 'EPres'], ['Extended_Pictographic', 'ExtPict'], ['Extender', 'Ext'],
  ['Grapheme_Base', 'Gr_Base'], ['Grapheme_Extend', 'Gr_Ext'], ['Hex_Digit', 'Hex'],
  ['IDS_Binary_Operator', 'IDSB'], ['IDS_Trinary_Operator', 'IDST'], ['ID_Continue', 'IDC'],
  ['ID_Start', 'IDS'], ['Ideographic', 'Ideo'], ['Join_Control', 'Join_C'],
  ['Logical_Order_Exception', 'LOE'], ['Lowercase', 'Lower'], ['Math'],
  ['Noncharacter_Code_Point', 'NChar'], ['Pattern_Syntax', 'Pat_Syn'],
  ['Pattern_White_Space', 'Pat_WS'], ['Quotation_Mark', 'QMark'], ['Radical'],
  ['Regional_Indicator', 'RI'], ['Sentence_Terminal', 'STerm'], ['Soft_Dotted', 'SD'],
  ['Terminal_Punctuation', 'Term'], ['Unified_Ideograph', 'UIdeo'], ['Uppercase', 'Upper'],
  ['Variation_Selector', 'VS'], ['White_Space', 'space'], ['XID_Continue', 'XIDC'],
  ['XID_Start', 'XIDS'],
];

const STRING_PROPS = [
  'Basic_Emoji', 'Emoji_Keycap_Sequence', 'RGI_Emoji_Modifier_Sequence', 'RGI_Emoji_Flag_Sequence',
  'RGI_Emoji_Tag_Sequence', 'RGI_Emoji_ZWJ_Sequence',
];

const hex = (n) => '0x' + n.toString(16).toUpperCase();
const ident = (s) => s.toUpperCase().replace(/[^A-Z0-9]/g, '_');

function emitRanges(name, list) {
  const parts = list.map(([a, b]) => `(${hex(a)}, ${hex(b)})`);
  const lines = [];
  for (let i = 0; i < parts.length; i += 8) lines.push('    ' + parts.slice(i, i + 8).join(', ') + ',');
  return `static ${name}: &[(u32, u32)] = &[\n${lines.join('\n')}\n];\n`;
}

function emitPairs(name, map) {
  const entries = [...map.entries()].sort((a, b) => a[0] - b[0]);
  const parts = entries.map(([a, b]) => `(${hex(a)}, ${hex(b)})`);
  const lines = [];
  for (let i = 0; i < parts.length; i += 8) lines.push('    ' + parts.slice(i, i + 8).join(', ') + ',');
  return `pub static ${name}: &[(u32, u32)] = &[\n${lines.join('\n')}\n];\n`;
}

async function main() {
  const text = {};
  for (const k of Object.keys(FILES)) text[k] = await load(k);

  // ---- General_Category -------------------------------------------------
  const gc = new Map();
  const upper = new Map();
  const assigned = new Ranges();
  {
    let first = null;
    for (const f of rows(text.UnicodeData)) {
      const cp = parseInt(f[0], 16);
      const cat = f[2];
      let lo = cp;
      if (f[1].endsWith(', First>')) {
        first = cp;
        continue;
      }
      if (f[1].endsWith(', Last>')) lo = first;
      if (!gc.has(cat)) gc.set(cat, new Ranges());
      gc.get(cat).add(lo, cp);
      assigned.add(lo, cp);
      if (f[12]) upper.set(cp, [parseInt(f[12], 16)]);
    }
    const cn = new Ranges();
    let next = 0;
    for (const [lo, hi] of assigned.normalized()) {
      if (lo > next) cn.add(next, lo - 1);
      next = hi + 1;
    }
    if (next <= 0x10ffff) cn.add(next, 0x10ffff);
    gc.set('Cn', cn);
  }
  const gcAliases = [];
  const scAliases = [];
  for (const f of rows(text.PropertyValueAliases)) {
    if (f[0] === 'gc') gcAliases.push(f.slice(1));
    if (f[0] === 'sc') scAliases.push(f.slice(1));
  }

  // ---- binary properties ------------------------------------------------
  const wanted = new Set(BINARY.map((b) => b[0]));
  const binary = new Map();
  for (const k of ['DerivedCoreProperties', 'PropList', 'DerivedBinaryProperties', 'DerivedNormalizationProps', 'EmojiData']) {
    for (const [name, set] of propertyFile(text[k], wanted)) binary.set(name, set);
  }
  for (const [name] of BINARY) if (!binary.has(name)) throw new Error(`no data for ${name}`);

  // ---- scripts ------------------------------------------------------------
  const scLong = new Map(); // short -> long
  for (const a of scAliases) scLong.set(a[0], a[1]);
  const longShort = new Map([...scLong].map(([s, l]) => [l, s]));
  const scriptOf = new Map(); // long name -> Ranges
  for (const [r, name] of rows(text.Scripts)) {
    if (!scriptOf.has(name)) scriptOf.set(name, new Ranges());
    const [lo, hi] = range(r);
    scriptOf.get(name).add(lo, hi);
  }
  {
    // Unknown (Zzzz) is every code point no script lists.
    const known = new Ranges();
    for (const s of scriptOf.values()) for (const [a, b] of s.list) known.add(a, b);
    const unk = new Ranges();
    let next = 0;
    for (const [lo, hi] of known.normalized()) {
      if (lo > next) unk.add(next, lo - 1);
      next = hi + 1;
    }
    if (next <= 0x10ffff) unk.add(next, 0x10ffff);
    scriptOf.set('Unknown', unk);
  }
  // Script_Extensions: the listed code points get their listed scripts;
  // every other code point has scx = {sc}.
  const scxListed = new Ranges();
  const scxExtra = new Map(); // long name -> Ranges
  for (const [r, names] of rows(text.ScriptExtensions)) {
    const [lo, hi] = range(r);
    scxListed.add(lo, hi);
    for (const short of names.split(/\s+/)) {
      const long = scLong.get(short);
      if (!long) throw new Error(`unknown script ${short}`);
      if (!scxExtra.has(long)) scxExtra.set(long, new Ranges());
      scxExtra.get(long).add(lo, hi);
    }
  }
  const listed = scxListed.normalized();
  const minus = (a, b) => {
    const res = [];
    let j = 0;
    for (let [lo, hi] of a) {
      while (j < b.length && b[j][1] < lo) j++;
      let k = j;
      while (lo <= hi) {
        if (k >= b.length || b[k][0] > hi) {
          res.push([lo, hi]);
          break;
        }
        if (b[k][0] > lo) res.push([lo, b[k][0] - 1]);
        lo = Math.max(lo, b[k][1] + 1);
        k++;
      }
    }
    return res;
  };

  // ---- case data ----------------------------------------------------------
  const fold = new Map();
  for (const [c, status, to] of rows(text.CaseFolding)) {
    if (status === 'C' || status === 'S') fold.set(parseInt(c, 16), parseInt(to, 16));
  }
  for (const f of rows(text.SpecialCasing)) {
    // code; lower; title; upper; (conditions;)? — skip conditional ones.
    if (f.length > 4 && f[4] !== '') continue;
    upper.set(parseInt(f[0], 16), f[3].split(/\s+/).filter(Boolean).map((x) => parseInt(x, 16)));
  }
  // The Canonicalize of non-unicode mode: toUppercase when it is one code
  // unit and does not map a non-ASCII unit into ASCII.
  const canon = new Map();
  for (const [c, u] of upper) {
    if (c > 0xffff || u.length !== 1 || u[0] > 0xffff || u[0] === c) continue;
    if (c >= 128 && u[0] < 128) continue;
    canon.set(c, u[0]);
  }

  // ---- properties of strings ----------------------------------------------
  const strProps = new Map(STRING_PROPS.map((n) => [n, { singles: new Ranges(), strings: [] }]));
  for (const k of ['EmojiSequences', 'EmojiZwjSequences']) {
    for (const [cps, type] of rows(text[k])) {
      const p = strProps.get(type);
      if (!p) throw new Error(`unknown sequence type ${type}`);
      if (cps.includes('..')) {
        const [lo, hi] = range(cps);
        p.singles.add(lo, hi);
      } else {
        const seq = cps.split(/\s+/).map((x) => parseInt(x, 16));
        if (seq.length === 1) p.singles.add(seq[0], seq[0]);
        else p.strings.push(seq);
      }
    }
  }

  // ---- emit -----------------------------------------------------------------
  let rs = `// DO NOT EDIT. Generated by tools/gen-regex-unicode.mjs from the Unicode
// Character Database ${version} (TASK-100.31).

/// The Unicode version of these tables.
pub const UNICODE_VERSION: &str = "${version}";

`;
  const gcBase = [...gc.keys()].sort();
  for (const k of gcBase) rs += emitRanges(`GC_${ident(k)}`, gc.get(k).normalized());
  rs += `\n/// General_Category values that name one category, by short name.\npub static GENERAL_CATEGORY: &[(&str, &[(u32, u32)])] = &[\n`;
  for (const k of gcBase) rs += `    ("${k}", GC_${ident(k)}),\n`;
  rs += `];\n\n/// General_Category value aliases (short name first), from PropertyValueAliases.txt.\npub static GENERAL_CATEGORY_ALIASES: &[&[&str]] = &[\n`;
  for (const a of gcAliases) rs += `    &[${a.map((x) => `"${x}"`).join(', ')}],\n`;
  rs += `];\n\n`;

  for (const [name] of BINARY) rs += emitRanges(`BIN_${ident(name)}`, binary.get(name).normalized());
  rs += `\n/// Binary properties (ES2025 table 69 less Any, ASCII and Assigned) with their aliases.\npub static BINARY: &[(&[&str], &[(u32, u32)])] = &[\n`;
  for (const b of BINARY) rs += `    (&[${b.map((x) => `"${x}"`).join(', ')}], BIN_${ident(b[0])}),\n`;
  rs += `];\n\n`;

  const scripts = [...scriptOf.keys()].sort();
  for (const s of scripts) {
    const sc = scriptOf.get(s).normalized();
    const scx = new Ranges();
    for (const [a, b] of minus(sc, listed)) scx.add(a, b);
    for (const [a, b] of scxExtra.get(s)?.list ?? []) scx.add(a, b);
    rs += emitRanges(`SC_${ident(s)}`, sc);
    rs += emitRanges(`SCX_${ident(s)}`, scx.normalized());
  }
  for (const s of scxExtra.keys()) if (!scriptOf.has(s)) throw new Error(`scx-only script ${s}`);
  rs += `\n/// Scripts: aliases (PropertyValueAliases.txt), Script ranges, Script_Extensions ranges.\npub static SCRIPTS: &[(&[&str], &[(u32, u32)], &[(u32, u32)])] = &[\n`;
  for (const s of scripts) {
    const a = scAliases.find((x) => x[1] === s);
    if (!a) throw new Error(`no aliases for ${s} (${longShort.get(s)})`);
    rs += `    (&[${a.map((x) => `"${x}"`).join(', ')}], SC_${ident(s)}, SCX_${ident(s)}),\n`;
  }
  rs += `];\n\n`;

  for (const n of STRING_PROPS) {
    const p = strProps.get(n);
    rs += emitRanges(`STR_${ident(n)}_SINGLES`, p.singles.normalized());
    rs += `static STR_${ident(n)}_STRINGS: &[&[u32]] = &[\n`;
    for (const s of p.strings) rs += `    &[${s.map(hex).join(', ')}],\n`;
    rs += `];\n`;
  }
  rs += `\n/// Properties of strings (v mode): single code points and longer strings.\npub static STRING_PROPERTIES: &[(&str, &[(u32, u32)], &[&[u32]])] = &[\n`;
  for (const n of STRING_PROPS) rs += `    ("${n}", STR_${ident(n)}_SINGLES, STR_${ident(n)}_STRINGS),\n`;
  rs += `];\n\n`;

  rs += `/// Simple case folding (CaseFolding.txt, statuses C and S): (c, scf(c)) for every c that changes.\n`;
  rs += emitPairs('SIMPLE_FOLD', fold);
  rs += `\n/// Canonicalize outside unicode mode: (c, toUppercase(c)) for every code unit whose\n/// full uppercase is one other code unit and does not map non-ASCII into ASCII.\n`;
  rs += emitPairs('CANON_UPPER', canon);

  fs.writeFileSync(out, rs);
  process.stderr.write(`wrote ${path.relative(root, out)} (${(rs.length / 1024).toFixed(0)} KiB)\n`);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
