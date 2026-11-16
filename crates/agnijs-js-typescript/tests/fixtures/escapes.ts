// A later `<` makes Oxc rewind past the bad escape and lose its error.
let a\x41 = 1, b = 2 << 1;
let c = c\u{0041} + b;
