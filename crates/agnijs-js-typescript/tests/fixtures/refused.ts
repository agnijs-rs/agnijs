// Constructs that would not run on the engine are refused in both modes.
@standard
class A {
  accessor field = 1;
}
import fs = require("fs");
import type ok = require("types-only");
export = A;
