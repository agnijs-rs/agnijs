// TypeScript 5.x syntax and recent ECMAScript, which passes through.
function first<const T extends readonly unknown[]>(x: T): T[0] {
  return x[0];
}
interface Box<in out T> {
  value: T;
}
export type * from "./types.js";
type Head<T> = T extends [infer H extends string, ...infer _] ? H : never;
class Base {
  m(): void {}
}
class Derived extends Base {
  override m(): void {}
  declare readonly tag: unique symbol;
  static #count = 0;
  static {
    Derived.#count++;
  }
}
type Tpl = `a${string}`;
const config = { retries: 3 } satisfies Record<string, number>;
const one = first<[1]>([1]);
export { type Box, config, one };
export async function work(): Promise<number> {
  await using resource = { async [Symbol.asyncDispose]() {} };
  using sync = { [Symbol.dispose]() {} };
  const m = await import("./m.js", { with: { type: "json" } });
  return /(?<year>\d{4})/v.exec("2026")?.groups?.year?.length ?? 0n as unknown as number;
}
