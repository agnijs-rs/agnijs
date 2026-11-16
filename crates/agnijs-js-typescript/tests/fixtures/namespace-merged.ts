// tsc resolves a bare name that another block of the same namespace
// exports; Oxc's transform would leave it unbound, so it is refused.
namespace N {
  export const a = 1;
  export function f() {
    return a;
  }
}
namespace N {
  export const b = a + f();
  const own = 2;
  export namespace Inner {
    export const c = own + a;
  }
}
namespace N {
  const a = "shadowed in this block";
  export const d = a + b;
}
