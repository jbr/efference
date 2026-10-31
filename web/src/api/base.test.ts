// The control base (api/base.ts) and what joins onto it. The derivation is a
// pure function of the page's path, so most of this needs no location at all;
// the join is exercised through the api layer itself, since "every control path
// joins onto the base" is a property of the wrapper rather than of one route.

// jsdom for the two tests that need a location to have been served at.

// @vitest-environment jsdom

import { afterEach, expect, test, vi } from "vitest";
import { deriveBase, deriveRoute } from "./base.ts";
import {
  claimSignIn,
  continueLoop,
  fetchSessions,
  requestSignIn,
} from "./http.ts";

afterEach(() => {
  vi.unstubAllGlobals();
  history.pushState({}, "", "/");
});

test("a page served at a root has an empty base", () => {
  for (const path of ["/", "/tree"]) {
    expect(deriveBase(path)).toBe("");
  }
});

test("a page served under a session prefix has that prefix as its base", () => {
  for (const path of [
    "/sessions/work",
    "/sessions/work/",
    "/sessions/work/tree",
  ]) {
    expect(deriveBase(path)).toBe("/sessions/work");
  }
  // The id shape the host resolves by (plain_id, src/session_dir.rs).
  expect(deriveBase("/sessions/1754300000000/")).toBe(
    "/sessions/1754300000000",
  );
});

test("a path that names no session id is not a prefix", () => {
  // `/sessions` is the host's own listing, not a base; the rest are ids the
  // host would refuse, so a page at one of them addresses the root.
  for (const path of ["/sessions", "/sessions/", "/sessions/../etc"]) {
    expect(deriveBase(path)).toBe("");
  }
});

test("the client route is the path with the base removed", () => {
  expect(deriveRoute("/")).toBe("/");
  expect(deriveRoute("/tree")).toBe("/tree");
  expect(deriveRoute("/sessions/work")).toBe("/");
  expect(deriveRoute("/sessions/work/")).toBe("/");
  expect(deriveRoute("/sessions/work/tree")).toBe("/tree");
});

test("control paths join onto the base; the listener's own do not", async () => {
  const urls: string[] = [];
  // A fresh Response per call: a body can only be read once.
  vi.stubGlobal(
    "fetch",
    vi.fn((...args: unknown[]) => {
      urls.push(String(args[0]));
      return Promise.resolve(new Response("{}", { status: 200 }));
    }),
  );

  history.pushState({}, "", "/sessions/work/");
  await continueLoop();
  await requestSignIn("Safari on macOS");
  await claimSignIn("claim", "Safari on macOS");
  await fetchSessions();

  expect(urls).toEqual([
    // The session's own API, behind the prefix a host routes.
    "/sessions/work/v1/loop/continue",
    // Sign-in is the listener's surface, and one credential covers every
    // session behind a host (docs/web-client.md).
    "/pair",
    "/pair/claim",
    // So is discovery.
    "/sessions",
  ]);

  history.pushState({}, "", "/");
  await continueLoop();
  expect(urls[4]).toBe("/v1/loop/continue");
});
