// Imports and exports belong at the top level or in a namespace.
namespace N { export const a = 1; }
function f() {
  export const b = 2;
}
{
  import x from "./x.js";
}
label: export {};
export const c = N.a;
