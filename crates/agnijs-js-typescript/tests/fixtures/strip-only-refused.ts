// @options: strip-only decorators
// Every construct with run-time meaning is refused, with its position.
enum E {
  A,
}
const enum F {
  B,
}
namespace N {
  export const x = 1;
}
class C {
  constructor(public a: number, readonly b: string) {}
  @dec m() {}
}
import alias = N.x;
