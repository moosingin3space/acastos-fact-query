"use strict";

// Marks each given output directory as ES modules. The package itself is
// CommonJS (the Node build), so the Workers build's ESM output and wasm-bindgen
// `--target web` glue each get a nested package.json, which Node, bundlers, and
// workerd tooling all honour.
const { mkdirSync, writeFileSync } = require("node:fs");
const { join } = require("node:path");

for (const dir of process.argv.slice(2)) {
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, "package.json"), JSON.stringify({ type: "module" }) + "\n");
}
