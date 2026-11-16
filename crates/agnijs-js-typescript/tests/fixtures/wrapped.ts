// Enum initializers and namespace bodies become functions. Oxc's parser
// refuses `await` in them, except in an exported enum or namespace.
const p = Promise.resolve(1);
export enum E { A = await p }
export namespace N {
  export const b = await p;
  export async function fine() { return [await p, arguments]; }
  for await (const x of []) {}
  export const f = () => arguments;
}
