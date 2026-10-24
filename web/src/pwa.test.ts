import { expect, it } from "vitest";
import { BEARER_DB, BEARER_KEY, BEARER_STORE } from "./api/bearer.ts";
import { workerUrl } from "./pwa.ts";
import worker from "../public/sw.js?raw";

it("names the non-caching worker for the dev path only", () => {
  expect(workerUrl(true)).toBe("/sw.js");
  expect(workerUrl(false)).toBe("/sw.js?dev=1");
});

it("clamps notification-click targets to the worker's origin", () => {
  // The worker is served raw from public/, unbundled, so nothing imports it —
  // the pin is textual for the same reason the wire-bytes pins are: it fails
  // when the clamp is deleted, and is updated deliberately when it moves. The
  // push payload's `path` is the sender's choice; without the clamp an
  // absolute URL would carry `openWindow` off-origin.
  expect(worker).toContain("if (target.origin !== self.location.origin)");
});

it("reads the bearer where the page keeps it", () => {
  // Two spellings of one place, the page's module (api/bearer.ts) and this
  // unbundled worker, with nothing but this pin connecting them: a name that
  // drifts in either reads as no bearer stored, and the push repair fails
  // soft where nothing reports it.
  expect(worker).toContain(`const BEARER_DB = ${JSON.stringify(BEARER_DB)};`);
  expect(worker).toContain(
    `const BEARER_STORE = ${JSON.stringify(BEARER_STORE)};`,
  );
  expect(worker).toContain(`const BEARER_KEY = ${JSON.stringify(BEARER_KEY)};`);
  expect(worker).toContain("self.indexedDB.open(BEARER_DB)");
  expect(worker).toContain(".transaction(BEARER_STORE)");
  expect(worker).toContain(".get(BEARER_KEY)");
});

it("sends the bearer on each of the push repair's requests", () => {
  // `resubscribe` makes its own requests under the `/v1` gate
  // (docs/web-push.md): the key read, the new subscription, and the old
  // endpoint's removal. Each carries the stored bearer.
  const repair = worker.slice(worker.indexOf("async function resubscribe("));
  expect(repair.match(/\bfetch\(/g)).toHaveLength(3);
  expect(repair).toContain("fetch(`${base}/v1/push`, { headers: credential })");
  expect(repair.match(/headers: \{ \.\.\.credential, /g)).toHaveLength(2);
  expect(repair).toContain(
    "token === null ? {} : { authorization: `Bearer ${token}` }",
  );
});
