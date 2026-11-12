// The agnijs command's prelude. Evaluated once per engine, it returns the
// installer function; everything else stays inside this closure, so the only
// globals it adds are the timer functions and `queueMicrotask`.
//
// install(host) is called with the host functions the command provides:
//   host.promiseState(p)  -> ["pending"] | ["fulfilled", v] | ["rejected", r]
//   host.setTimer(f, ms, repeat, args) -> id
//   host.clearTimer(id)
//   host.patchConsole     -> whether console.* should format with inspect
// It returns { inspect, format }.
(function () {
  'use strict';
  const ObjectProto = Object.prototype;
  const toStr = (v) => ObjectProto.toString.call(v);
  const hasOwn = (o, k) => ObjectProto.hasOwnProperty.call(o, k);
  const IDENT = /^[A-Za-z_$][\w$]*$/;
  const MAX_ITEMS = 100;
  const MAX_DEPTH = 2;
  const LINE = 72;

  let promiseState = () => ['pending'];

  function quote(s) {
    const q = s.includes("'") && !s.includes('"') ? '"' : "'";
    let out = q;
    for (const ch of s) {
      const c = ch.codePointAt(0);
      if (ch === q) out += '\\' + q;
      else if (ch === '\\') out += '\\\\';
      else if (ch === '\n') out += '\\n';
      else if (ch === '\t') out += '\\t';
      else if (ch === '\r') out += '\\r';
      else if (c < 0x20 || c === 0x7f) out += '\\x' + c.toString(16).padStart(2, '0');
      else out += ch;
    }
    return out + q;
  }

  function keyText(k) {
    if (typeof k === 'symbol') return '[' + k.toString() + ']';
    return IDENT.test(k) ? k : quote(k);
  }

  function fnText(f) {
    let src = '';
    try { src = Function.prototype.toString.call(f); } catch (e) { /* host function */ }
    const name = typeof f.name === 'string' && f.name !== '' ? f.name : '';
    if (/^class[\s{]/.test(src)) return name ? '[class ' + name + ']' : '[class (anonymous)]';
    const kind = /^async\s/.test(src)
      ? (/^async\s*function\s*\*/.test(src) ? 'AsyncGeneratorFunction' : 'AsyncFunction')
      : /^function\s*\*/.test(src) ? 'GeneratorFunction' : 'Function';
    return '[' + kind + (name ? ': ' + name : ' (anonymous)') + ']';
  }

  function ctorName(o) {
    let p;
    try { p = Object.getPrototypeOf(o); } catch (e) { return ''; }
    if (p === null) return null;
    try {
      const c = p.constructor;
      if (typeof c === 'function' && typeof c.name === 'string') return c.name;
    } catch (e) { /* a throwing getter */ }
    return '';
  }

  // Join entries on one line when they fit, else one per line.
  function wrap(prefix, open, entries, close, indent) {
    if (entries.length === 0) return prefix + open + close;
    const one = prefix + open + ' ' + entries.join(', ') + ' ' + close;
    if (one.length + indent.length <= LINE && !one.includes('\n')) return one;
    const pad = indent + '  ';
    return prefix + open + '\n' + entries.map((e) => pad + e).join(',\n') + '\n' + indent + close;
  }

  function inspect(v, opts) {
    const seen = [];
    const top = opts && opts.top;
    return show(v, 0, '', top);

    function show(v, depth, indent, raw) {
      switch (typeof v) {
        case 'string': return raw ? v : quote(v);
        case 'number': return Object.is(v, -0) ? '-0' : String(v);
        case 'bigint': return String(v) + 'n';
        case 'boolean': return String(v);
        case 'undefined': return 'undefined';
        case 'symbol': return v.toString();
        case 'function': if (depth > 0 || ownKeys(v).length === 0) return fnText(v);
      }
      if (v === null) return 'null';
      if (seen.includes(v)) return '[Circular]';
      seen.push(v);
      try { return object(v, depth, indent); } finally { seen.pop(); }
    }

    function ownKeys(o) {
      try {
        return Reflect.ownKeys(o).filter((k) => {
          const d = Object.getOwnPropertyDescriptor(o, k);
          return d && d.enumerable;
        });
      } catch (e) { return []; }
    }

    function props(o, depth, indent, skip) {
      const out = [];
      for (const k of ownKeys(o)) {
        if (skip && skip(k)) continue;
        const d = Object.getOwnPropertyDescriptor(o, k);
        let text;
        if (d.get || d.set) text = d.get && d.set ? '[Getter/Setter]' : d.get ? '[Getter]' : '[Setter]';
        else text = show(d.value, depth + 1, indent + '  ');
        out.push(keyText(k) + ': ' + text);
      }
      return out;
    }

    function object(o, depth, indent) {
      const tag = toStr(o);
      const name = ctorName(o);
      const nested = () => depth > MAX_DEPTH;
      if (typeof o === 'function') {
        return wrap(fnText(o) + ' ', '{', props(o, depth, indent), '}', indent);
      }
      if (o instanceof Error || tag === '[object Error]') {
        let s;
        try { s = typeof o.stack === 'string' && o.stack ? o.stack : String(o); } catch (e) { s = '[Error]'; }
        if (depth > 0) s = s.split('\n').map((l, i) => (i ? indent + l : l)).join('\n');
        const extra = props(o, depth, indent, (k) => k === 'stack' || k === 'message');
        return extra.length ? s + ' ' + wrap('', '{', extra, '}', indent) : s;
      }
      if (Array.isArray(o)) {
        if (nested()) return '[Array]';
        const items = [];
        const n = o.length;
        for (let i = 0; i < Math.min(n, MAX_ITEMS); i++) {
          items.push(i in o ? show(o[i], depth + 1, indent + '  ') : '<empty>');
        }
        if (n > MAX_ITEMS) items.push('... ' + (n - MAX_ITEMS) + ' more items');
        items.push(...props(o, depth, indent, (k) => typeof k === 'string' && /^\d+$/.test(k)));
        const prefix = name && name !== 'Array' ? name + '(' + n + ') ' : '';
        return wrap(prefix, '[', items, ']', indent);
      }
      if (ArrayBuffer.isView(o) && !(o instanceof DataView)) {
        if (nested()) return '[' + name + ']';
        const items = [];
        for (let i = 0; i < Math.min(o.length, MAX_ITEMS); i++) items.push(show(o[i], depth + 1, indent));
        if (o.length > MAX_ITEMS) items.push('... ' + (o.length - MAX_ITEMS) + ' more items');
        return wrap(name + '(' + o.length + ') ', '[', items, ']', indent);
      }
      if (o instanceof ArrayBuffer) return 'ArrayBuffer { byteLength: ' + o.byteLength + ' }';
      if (o instanceof Date) {
        const t = o.getTime();
        return t === t ? o.toISOString() : 'Invalid Date';
      }
      if (o instanceof RegExp) return String(o);
      if (o instanceof Map) {
        if (nested()) return '[Map]';
        const items = [];
        for (const [k, val] of o) {
          if (items.length >= MAX_ITEMS) { items.push('... more items'); break; }
          items.push(show(k, depth + 1, indent + '  ') + ' => ' + show(val, depth + 1, indent + '  '));
        }
        return wrap((name || 'Map') + '(' + o.size + ') ', '{', items, '}', indent);
      }
      if (o instanceof Set) {
        if (nested()) return '[Set]';
        const items = [];
        for (const val of o) {
          if (items.length >= MAX_ITEMS) { items.push('... more items'); break; }
          items.push(show(val, depth + 1, indent + '  '));
        }
        return wrap((name || 'Set') + '(' + o.size + ') ', '{', items, '}', indent);
      }
      if (o instanceof WeakMap) return 'WeakMap { <items unknown> }';
      if (o instanceof WeakSet) return 'WeakSet { <items unknown> }';
      if (o instanceof Promise) {
        const st = promiseState(o);
        const inner = st[0] === 'pending' ? '<pending>'
          : st[0] === 'rejected' ? '<rejected> ' + show(st[1], depth + 1, indent + '  ')
          : show(st[1], depth + 1, indent + '  ');
        return 'Promise { ' + inner + ' }';
      }
      for (const [Box, label] of [[Number, 'Number'], [String, 'String'], [Boolean, 'Boolean'], [BigInt, 'BigInt'], [Symbol, 'Symbol']]) {
        if (tag === '[object ' + label + ']' && !(label === 'Symbol' && typeof o === 'symbol')) {
          try {
            const prim = Box.prototype.valueOf.call(o);
            return '[' + label + ': ' + show(prim, depth + 1, indent) + ']';
          } catch (e) { /* not a box */ }
        }
      }
      if (nested()) return name === null ? '[Object: null prototype]' : '[' + (name || 'Object') + ']';
      const entries = props(o, depth, indent);
      let prefix = '';
      if (name === null) prefix = '[Object: null prototype] ';
      else if (name && name !== 'Object') prefix = name + ' ';
      else if (name === '') {
        const t = tag.slice(8, -1);
        if (t !== 'Object') prefix = t + ' ';
      }
      const st = o[Symbol.toStringTag];
      if (typeof st === 'string' && st !== '' && !prefix.startsWith(st)) prefix = (prefix ? prefix.trim() + ' ' : 'Object ') + '[' + st + '] ';
      return wrap(prefix, '{', entries, '}', indent);
    }
  }

  // console-style formatting: printf directives in a leading string, then
  // every other argument inspected (strings as they are).
  function format(args) {
    let i = 0;
    let out = '';
    if (typeof args[0] === 'string' && args.length > 1) {
      const f = args[0];
      i = 1;
      for (let j = 0; j < f.length; j++) {
        const c = f[j];
        if (c !== '%' || j + 1 >= f.length) { out += c; continue; }
        const d = f[++j];
        if (d === '%') { out += '%'; continue; }
        if (!'sdifjoOc'.includes(d)) { out += '%' + d; continue; }
        if (i >= args.length) { out += '%' + d; continue; }
        const a = args[i++];
        switch (d) {
          case 's': out += typeof a === 'string' ? a : typeof a === 'object' && a !== null ? inspect(a) : show1(a); break;
          case 'd': case 'i': {
            if (typeof a === 'bigint') { out += a + 'n'; break; }
            const n = Number(a);
            out += d === 'i' ? String(Math.trunc(n)) : String(n);
            break;
          }
          case 'f': out += String(parseFloat(a)); break;
          case 'j': try { out += JSON.stringify(a); } catch (e) { out += '[Circular]'; } break;
          case 'o': case 'O': out += inspect(a); break;
          case 'c': break;
        }
      }
    } else if (args.length > 0) {
      out = typeof args[0] === 'string' ? args[0] : inspect(args[0]);
      i = 1;
    }
    for (; i < args.length; i++) {
      const a = args[i];
      out += ' ' + (typeof a === 'string' ? a : inspect(a));
    }
    return out;
  }
  function show1(v) { return typeof v === 'symbol' ? v.toString() : inspect(v); }

  return function install(host) {
    promiseState = host.promiseState;
    const g = globalThis;
    const define = (name, value) => Object.defineProperty(g, name, { value, writable: true, configurable: true, enumerable: false });
    const setTimer = host.setTimer;
    const clearTimer = host.clearTimer;
    const check = (f) => {
      if (typeof f !== 'function') throw new TypeError('The "callback" argument must be of type function');
      return f;
    };
    define('setTimeout', function setTimeout(f, ms, ...args) { return setTimer(check(f), +ms || 0, false, args); });
    define('setInterval', function setInterval(f, ms, ...args) { return setTimer(check(f), +ms || 0, true, args); });
    define('clearTimeout', function clearTimeout(id) { if (id !== undefined) clearTimer(+id); });
    define('clearInterval', function clearInterval(id) { if (id !== undefined) clearTimer(+id); });
    define('queueMicrotask', function queueMicrotask(f) { check(f); Promise.resolve().then(() => f()); });
    if (host.patchConsole && typeof g.console === 'object') {
      const c = g.console;
      for (const level of ['log', 'info', 'warn', 'error', 'debug']) {
        const orig = c[level];
        if (typeof orig !== 'function') continue;
        c[level] = { [level](...args) { return orig.call(c, format(args)); } }[level];
      }
      if (typeof c.dir !== 'function') c.dir = function dir(v) { return c.log(inspect(v)); };
    }
    return { inspect, format };
  };
})()
