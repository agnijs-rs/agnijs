// The bytes behind typed arrays: little-endian, and one NaN bit pattern
// whatever arithmetic made the NaN (x86 and ARM make different ones).
var out = [];
const bytes = (ta) => Array.from(new Uint8Array(ta.buffer)).join(",");
const zero = Number("0");
const inf = Number("Infinity");
out.push(bytes(new Float64Array([zero / zero, -(zero / zero), inf - inf, Math.sqrt(-1), NaN])));
out.push(bytes(new Float32Array([zero / zero, -(zero / zero), NaN])));
const dv = new DataView(new ArrayBuffer(16));
dv.setFloat64(0, -(zero / zero));
dv.setFloat32(8, zero / zero, true);
dv.setFloat16?.(12, zero / zero);
out.push(Array.from(new Uint8Array(dv.buffer)).join(","));
out.push(bytes(new Float64Array([0.1, -0, 1e308 * 10])));
out.push(bytes(new Float32Array([1.1, 16777217])));
out.push(bytes(new Int32Array([-2, 0x12345678])));
out.push(bytes(new BigInt64Array([-1n, 2n ** 62n])));
out.push(Array.from(new Uint8ClampedArray([-5, 1.5, 2.5, 300])).join(","));
const f = new Float64Array([zero / zero]);
out.push(bytes(new Float64Array(f)) + " " + bytes(new Float32Array(f)));
