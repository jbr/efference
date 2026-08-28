// Which binding served the page (session/entry.ts) — the one thing the client
// cannot read off its own URL when that URL is a root. jsdom, because the answer
// under a session prefix is read from the location rather than fetched.

// @vitest-environment jsdom

import { afterEach, expect, test, vi } from "vitest";
import { resolveEntry } from "./entry.ts";

const json = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), { status });

afterEach(() => {
  vi.unstubAllGlobals();
  history.pushState({}, "", "/");
});

test("a session prefix is the session's view, with nothing asked", async () => {
  const fetchMock = vi.fn();
  vi.stubGlobal("fetch", fetchMock);
  history.pushState({}, "", "/sessions/work/");

  expect(await resolveEntry()).toEqual({ kind: "session" });
  expect(fetchMock).not.toHaveBeenCalled();
});

test("a listing at the root is the host, and its sessions are the rows", async () => {
  vi.stubGlobal(
    "fetch",
    vi.fn(() => Promise.resolve(json(200, [{ id: "work" }, { id: "spike" }]))),
  );

  expect(await resolveEntry()).toEqual({
    kind: "list",
    sessions: [{ id: "work" }, { id: "spike" }],
  });
});

test("a root that serves no listing is a session daemon's", async () => {
  vi.stubGlobal(
    "fetch",
    vi.fn(() =>
      Promise.resolve(
        json(404, { code: "not_found", message: "no route matched" }),
      ),
    ),
  );

  expect(await resolveEntry()).toEqual({ kind: "session" });
});

test("a guarded listing is the host, unauthenticated", async () => {
  vi.stubGlobal(
    "fetch",
    vi.fn(() =>
      Promise.resolve(
        json(401, { code: "unauthorized", message: "no valid session cookie" }),
      ),
    ),
  );

  expect(await resolveEntry()).toEqual({ kind: "unauthenticated" });
});

test("a probe that does not arrive leaves the root a session", async () => {
  // Offline, or a daemon that went away: the session view's own connection
  // banner reports that, where a second failure screen would only restate it.
  vi.stubGlobal(
    "fetch",
    vi.fn(() => Promise.reject(new TypeError("network error"))),
  );

  expect(await resolveEntry()).toEqual({ kind: "session" });
});
