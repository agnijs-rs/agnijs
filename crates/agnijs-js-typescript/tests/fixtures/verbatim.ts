// @options: verbatim
import type { OnlyType } from "./types.js";
import { type Mixed, value } from "./values.js";
import { unusedAsValue } from "./unused.js";
let typed: unusedAsValue | OnlyType | Mixed = 1;
export const used = [value, typed];
