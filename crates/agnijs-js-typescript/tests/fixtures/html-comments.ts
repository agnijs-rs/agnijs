// Oxc's lexer reports an HTML-like comment in a module, but the error is
// lost when the TypeScript parser later rewinds at a `<`; the printer would
// keep the comment, which the engine refuses in a module.
<!-- not a comment in a module
enum E { A = 1 << 2 }
