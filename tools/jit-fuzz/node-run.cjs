// Run a file in Node as a classic script, as the agnijs command runs it: top-level `var` and
// function declarations become properties of the global object. (A file run with `node file.js`
// is a CommonJS module, where they would be module locals.) Used by jit-fuzz.mjs --node.
"use strict";
const fs = require("node:fs");
const vm = require("node:vm");

const file = process.argv[2];
try {
  vm.runInThisContext(fs.readFileSync(file, "utf8"), { filename: file });
} catch (e) {
  console.error("Uncaught " + (e && e.stack ? e.stack : String(e)));
  process.exitCode = 1;
}
