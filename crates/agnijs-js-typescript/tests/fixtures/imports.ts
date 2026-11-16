import type { OnlyType } from "./types.js";
import { type Mixed, value } from "./values.js";
import { unusedAsValue } from "./unused.js";
import Default, * as all from "./all.js";
import "./side-effect.js";
export type { OnlyType };
export { type Mixed };
export * from "./reexport.js";
export * as group from "./group.js";
export { value as renamed } from "./values.js";
export type { Other } from "./other.js";

let typed: unusedAsValue = 1;
export const used = [value, Default, all.member, typed];
