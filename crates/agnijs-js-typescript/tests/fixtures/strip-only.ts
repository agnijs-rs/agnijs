// @options: strip-only
// Erasable syntax only: strip-only accepts all of it.
import type { T } from "./t.js";
interface I {
  a: number;
}
type U = I | T;
declare enum Ambient {
  X,
}
declare namespace Lib {
  const v: number;
}
namespace TypesOnly {
  export type V = number;
  export interface W {}
}
class C<X> implements I {
  a: number = 1;
  declare b: string;
  m<Y>(x: X, y?: Y): asserts x is X {}
}
export const value = new C<number>() as I satisfies I;
