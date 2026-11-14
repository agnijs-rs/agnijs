// Number formatting and the arithmetic IEEE 754 pins down exactly. The
// transcendental Math functions are left out: they use the platform's libm
// (docs/determinism.md).
var out = [];
out.push(String(0.1 + 0.2));
out.push([1e21, 1e-7, 123e-20, -0, 2 ** 53 + 2, 5e-324, Number.MAX_VALUE].map(String).join(" "));
out.push([(123.456).toFixed(2), (0.000001234).toExponential(3), (1234.5678).toPrecision(6), (1e21).toFixed(2)].join(" "));
// Exact ties between two shortest forms take the even digit, as V8 does
// (2 ** -24's even neighbour does not round-trip).
out.push([2 ** -25, -(2 ** -23 * 5), 2 ** -24, (2 ** -25).toExponential(), 901134.0629882812].map(String).join(" "));
out.push([(255).toString(16), (0.5).toString(2), (-255.5).toString(36), (1 / 3).toString(7)].join(" "));
out.push([parseFloat("1e-7x"), parseInt("0x1F"), Number("  12.50  "), Number("0b101"), +"1_000"].join(" "));
out.push([Math.sqrt(2), Math.fround(0.1), Math.round(-2.5), Math.trunc(-0.9), Math.clz32(1), Math.imul(0x7fffffff, 3)].join(" "));
out.push([Math.abs(-7.25), Math.max(1, NaN), Math.min(-0, 0), Math.sign(-3), Math.cbrt(27), Math.hypot(3, 4)].join(" "));
out.push([7 % -3, -7 % 3, 2 ** -1074, 1 / -0, 0.1 * 3, 9007199254740993].join(" "));
out.push([(2n ** 100n).toString(), (-7n / 2n).toString(), (-7n % 2n).toString(), BigInt.asUintN(8, -1n).toString(), (255n).toString(2)].join(" "));
out.push([1 | 0x80000000, -1 >>> 0, 1 << 31, ~5, 5 ^ 3].join(" "));
