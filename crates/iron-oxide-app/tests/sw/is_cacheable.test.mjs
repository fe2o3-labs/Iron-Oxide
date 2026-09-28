// Unit tests for the caching rule in public/sw.js.
// Run with: node --test crates/iron-oxide-app/tests/sw/is_cacheable.test.mjs
//
// sw.js is a classic service worker script, not a module, so it is evaluated in a sandbox with a
// minimal `self`; its top-level function declarations become properties of the sandbox.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";

const source = readFileSync(new URL("../../public/sw.js", import.meta.url), "utf8");
const sandbox = {
  self: { location: { search: "?build=test", origin: "http://localhost" }, addEventListener() {} },
  URLSearchParams,
  URL,
};
vm.runInNewContext(source, sandbox);
const { isCacheable } = sandbox;

// A cacheable response, with any field overridden.
function response({ status = 200, redirected = false, type = "basic", contentType = "text/javascript" } = {}) {
  const headers = new Headers();
  if (contentType !== null) {
    headers.set("Content-Type", contentType);
  }
  return { status, redirected, type, headers };
}

test("caches a direct 200 JS, wasm, CSS or image response", () => {
  for (const contentType of ["text/javascript", "application/wasm", "text/css; charset=utf-8", "image/png"]) {
    assert.equal(isCacheable(response({ contentType })), true, contentType);
  }
});

test("never caches HTML or XHTML, whatever the case or parameters", () => {
  for (const contentType of ["text/html", "text/html; charset=utf-8", "Text/HTML", " text/html ", "application/xhtml+xml"]) {
    assert.equal(isCacheable(response({ contentType })), false, contentType);
  }
});

test("never caches a response without a Content-Type", () => {
  assert.equal(isCacheable(response({ contentType: null })), false);
  assert.equal(isCacheable(response({ contentType: "" })), false);
  assert.equal(isCacheable(response({ contentType: "; charset=utf-8" })), false);
});

test("never caches anything but a 200", () => {
  for (const status of [201, 204, 206, 304, 404, 500]) {
    assert.equal(isCacheable(response({ status })), false, String(status));
  }
});

test("never caches a redirected response", () => {
  assert.equal(isCacheable(response({ redirected: true })), false);
});

test("never caches an opaque or CORS response", () => {
  for (const type of ["opaque", "cors", "error"]) {
    assert.equal(isCacheable(response({ type })), false, type);
  }
});
