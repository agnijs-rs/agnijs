// Every binary and unary number operator over every pair of 23 edge values (signed zero, int32
// and uint32 bounds, 2^53, infinities, NaN, shift counts, a denormal).
function edges(a, b) { var r = []; r.push(a + b, a - b, a * b, a / b, a % b, a ** b, a << b, a >> b, a >>> b, a & b, a | b, a ^ b, a < b, a <= b, a == b, a === b, a != b, -a, ~a); return r.join(" "); }
var E = [0, -0, 1, -1, 0.5, 2147483647, 2147483648, -2147483648, -2147483649, 4294967295, 4294967296, 9007199254740992, 1e300, -1e300, Infinity, -Infinity, NaN, 31, 32, 33, 63, 64, 1e-320];
var L = [];
for (var i = 0; i < E.length; i++) for (var j = 0; j < E.length; j++) L.push(i + "," + j + ": " + edges(E[i], E[j]));
console.log(L.join("\n"));
