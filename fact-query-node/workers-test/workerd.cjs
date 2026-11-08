"use strict";

// Runs the Cloudflare Workers build inside workerd (via Miniflare). Not part of
// `npm test`: run with `npm run test:workers`.

const { test } = require("node:test");
const assert = require("node:assert/strict");
const path = require("node:path");
const { Miniflare } = require("miniflare");

const root = path.join(__dirname, "..");

test("the Workers build loads and evaluates inside workerd", async () => {
  const mf = new Miniflare({
    modules: true,
    scriptPath: path.join(__dirname, "worker.js"),
    modulesRoot: root,
    modulesRules: [
      { type: "ESModule", include: ["**/*.js"] },
      { type: "CompiledWasm", include: ["**/*.wasm"] },
    ],
    compatibilityDate: "2026-07-01",
  });
  try {
    const response = await mf.dispatchFetch("http://localhost/");
    const body = await response.json();
    assert.equal(response.status, 200, JSON.stringify(body));
    assert.deepEqual(body, {
      rows: [
        ["1", "2"],
        ["1", "3"],
        ["2", "3"],
      ],
      truncated: false,
      support: [1, 1, 1],
      schema: ["edge", "path"],
      errorStage: "schema",
    });
  } finally {
    await mf.dispose();
  }
});
