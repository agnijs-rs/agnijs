// A script runs no import or export; the type-only ones are erased.
import type { T } from "./types.js";
export type { T };
import { value } from "./value.js";
export const v: T = value;
export {};
