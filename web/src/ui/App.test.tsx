// @vitest-environment jsdom
// A bearer the host refuses, seen through the whole app (docs/web-client.md).
// The fake host answers by bearer, so one revoked at the host is refused wherever
// it is next sent: the listing stream, an attention report, a session's event
// stream, the archived-sessions read. Each is followed by the sign-in view in the
// place of the view that asked, with the connections that view held closed and
// nothing sent under the dead bearer afterwards; a bearer stored by signing in
// brings the page's view back, connected anew.

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { bearer } from "../api/bearer.ts";
import { HEARTBEAT_MS, startAttention, TICK_MS } from "../attention.ts";
import { acquireStore, forgetStore } from "../session/registry.ts";
import { App } from "./App.tsx";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const encoder = new TextEncoder();

/** The one session the fake host lists. */
const LISTING = [
  { id: "work", title: "Work", status: "idle", processes: 0, cwd: "/work" },
];

const failure = (status: number) =>
  Response.json(
    status === 401
      ? {
          code: "unauthorized",
          message: "no valid `Authorization: Bearer` device token",
        }
      : { code: "internal", message: "the host failed" },
    { status },
  );

/** A host that accepts the bearers in `valid` and records every request it is
 * sent. Its event streams stay open until `drop()` ends them, as a dropped
 * connection does, or the request's signal aborts them. */
function fakeHost() {
  const valid = new Set<string>();
  const seen: { path: string; bearer: string | null; stream: boolean }[] = [];
  const open = new Set<ReadableStreamDefaultController<Uint8Array>>();
  const delayed: (() => void)[] = [];
  let approved: string | null = null;
  let holding: (bearer: string | null) => boolean = () => false;

  function live(signal: AbortSignal | null | undefined, first = "") {
    let controller!: ReadableStreamDefaultController<Uint8Array>;
    const body = new ReadableStream<Uint8Array>({
      start(c) {
        controller = c;
        if (first) c.enqueue(encoder.encode(first));
      },
    });
    open.add(controller);
    signal?.addEventListener("abort", () => {
      open.delete(controller);
      try {
        controller.error(new DOMException("aborted", "AbortError"));
      } catch {
        // Already ended; there is nothing left to abort.
      }
    });
    return new Response(body, { status: 200 });
  }

  function route(
    url: URL,
    token: string | null,
    stream: boolean,
    signal: AbortSignal | null | undefined,
  ): Response {
    if (url.pathname === "/pair") {
      return Response.json({ claim: "c1", code: "KQ7-M4X", expires_in: 90 });
    }
    if (url.pathname === "/pair/claim") {
      if (approved === null) {
        return Response.json({ status: "pending" }, { status: 202 });
      }
      valid.add(approved);
      return Response.json({ token: approved });
    }
    if (token === null || !valid.has(token)) return failure(401);
    const forced = host.force(url.pathname + url.search);
    if (forced !== null) return failure(forced);

    if (url.pathname === "/attention")
      return new Response(null, { status: 204 });
    if (url.pathname === "/sessions") {
      if (stream) return live(signal, `data: ${JSON.stringify(LISTING)}\n\n`);
      return Response.json(url.searchParams.has("state") ? [] : LISTING);
    }
    if (/^\/sessions\/[^/]+\/v1\/log$/.test(url.pathname)) {
      return Response.json([]);
    }
    if (/^\/sessions\/[^/]+\/v1\/events$/.test(url.pathname)) {
      return live(signal);
    }
    if (/^\/sessions\/[^/]+\/files\//.test(url.pathname)) {
      if (stream) return live(signal);
      return Response.json({
        path: "",
        recursive: false,
        truncated: false,
        entries: [],
      });
    }
    return Response.json(
      { code: "not_found", message: "no route matched" },
      { status: 404 },
    );
  }

  const host = {
    seen,
    valid,
    /** The status a route answers whatever the bearer, when it is not `null`. */
    force: (() => null) as (path: string) => number | null,
    /** How many event streams are open now. */
    open: () => open.size,
    /** End every open stream, as the host's side of a dropped connection. */
    drop() {
      for (const controller of open) controller.close();
      open.clear();
    },
    /** The operator approving the code on screen: the next claim collects it. */
    approve(token: string) {
      approved = token;
    },
    /** Hold back the answers to requests that carry a bearer `when` names, until
     * `release()`. */
    hold(when: (bearer: string | null) => boolean) {
      holding = when;
    },
    release() {
      delayed.splice(0).forEach((deliver) => deliver());
    },
    handle(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
      const url = new URL(String(input), location.href);
      const headers = new Headers(init?.headers);
      const token =
        headers.get("authorization")?.replace(/^Bearer /, "") ?? null;
      const stream = headers.get("accept") === "text/event-stream";
      seen.push({ path: url.pathname + url.search, bearer: token, stream });
      const response = route(url, token, stream, init?.signal);
      if (!holding(token)) return Promise.resolve(response);
      return new Promise((resolve) => delayed.push(() => resolve(response)));
    },
  };
  return host;
}

let host: ReturnType<typeof fakeHost>;
let root: Root;
let element: HTMLElement;

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubEnv("DEV", false);
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  vi.stubGlobal("matchMedia", () => ({
    matches: false,
    addEventListener: () => {},
    removeEventListener: () => {},
  }));
  const stored = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => stored.get(key) ?? null,
    setItem: (key: string, value: string) => void stored.set(key, value),
    removeItem: (key: string) => void stored.delete(key),
  });
  vi.spyOn(window, "scrollTo").mockImplementation(() => {});
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockImplementation(
    () => null,
  );
  vi.spyOn(document, "hasFocus").mockReturnValue(true);
  host = fakeHost();
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL, init?: RequestInit) =>
      host.handle(input, init),
    ),
  );
});

afterEach(async () => {
  act(() => root.unmount());
  element.remove();
  forgetStore("/sessions/work");
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.unstubAllEnvs();
  vi.restoreAllMocks();
  await bearer.clear();
  history.replaceState(null, "", "/");
});

/** Let the requests in flight answer and React render what they changed. */
async function settle(ms = 0) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

/** Advance until `check` holds: a request and its render chain through several
 * promises. */
async function until(check: () => boolean) {
  for (let step = 0; step < 100 && !check(); step++) await settle(50);
  expect(check()).toBe(true);
}

async function mount(path: string) {
  history.replaceState(null, "", path);
  element = document.body.appendChild(document.createElement("div"));
  root = createRoot(element);
  await act(async () => root.render(<App />));
  await settle();
}

const signInView = () => element.querySelector(".login");
const listView = () => element.querySelector(".overview");
const sessionView = () => element.querySelector(".session-main");
const sentWith = (token: string) =>
  host.seen.filter((request) => request.bearer === token);

test("a 401 to the listing stream shows the sign-in view in place of the list, and signing in brings the list back", async () => {
  host.valid.add("old");
  await bearer.store("old");
  await mount("/");
  await until(() => host.open() > 0);
  expect(listView()).not.toBeNull();
  expect(element.textContent).toContain("Work");

  // The device is revoked and the host's connections to it end. The list shows
  // the loss until the reconnect, which the host refuses.
  host.valid.delete("old");
  host.drop();
  await settle(10);
  expect(element.textContent).toContain("Lost the connection to the host");
  await settle(1500);

  expect(signInView()).not.toBeNull();
  expect(listView()).toBeNull();
  expect(element.textContent).not.toContain("Lost the connection");
  expect(bearer.held()).toBeNull();
  expect(host.open()).toBe(0);

  // Nothing is sent under the dead bearer however long the sign-in view waits.
  const sent = sentWith("old").length;
  await settle(60_000);
  expect(sentWith("old")).toHaveLength(sent);

  // The operator approves the code: the list mounts again, its connections
  // open under the new bearer.
  host.approve("new");
  await until(() => listView() !== null);
  expect(bearer.held()).toBe("new");
  expect(signInView()).toBeNull();
  await until(() => host.open() > 0);
  expect(
    sentWith("new").some(
      (request) => request.path === "/sessions" && request.stream,
    ),
  ).toBe(true);
});

test("a 401 to an attention report shows the sign-in view, reporting waits for the next sign-in, and then resumes", async () => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
  vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
  host.valid.add("old");
  await bearer.store("old");
  const stopAttention = startAttention();
  try {
    await mount("/");
    const reports = () =>
      host.seen.filter((request) => request.path === "/attention");
    window.dispatchEvent(new Event("pointerdown"));
    await until(() => reports().length === 1);
    expect(listView()).not.toBeNull();

    // Revoked: the next heartbeat is refused.
    host.valid.delete("old");
    await settle(HEARTBEAT_MS + TICK_MS);
    expect(reports()).toHaveLength(2);
    expect(signInView()).not.toBeNull();
    expect(bearer.held()).toBeNull();

    // Signed out, the person is still there and the clock still runs; nothing
    // is sent until a bearer is.
    window.dispatchEvent(new Event("pointerdown"));
    await settle(HEARTBEAT_MS);
    window.dispatchEvent(new Event("pointerdown"));
    await settle(HEARTBEAT_MS);
    expect(reports()).toHaveLength(2);

    host.approve("new");
    await until(() => reports().length === 3);
    expect(reports().at(-1)?.bearer).toBe("new");
  } finally {
    stopAttention();
  }
});

test("a 401 to a session's event stream shows the sign-in view in place of the session, and signing in connects it again", async () => {
  host.valid.add("old");
  await bearer.store("old");
  await mount("/sessions/work/");
  const connection = () => acquireStore("/sessions/work").snapshot().connection;
  await until(() => connection() === "live");
  expect(sessionView()).not.toBeNull();

  host.valid.delete("old");
  host.drop();
  await settle(10);
  expect(connection()).toBe("reconnecting");
  await settle(1500);

  expect(signInView()).not.toBeNull();
  expect(sessionView()).toBeNull();
  expect(bearer.held()).toBeNull();
  expect(host.open()).toBe(0);
  const sent = sentWith("old").length;
  await settle(60_000);
  expect(sentWith("old")).toHaveLength(sent);

  // The session was refused mid-stream; once signed in it reads again from its
  // warm store, over a fresh connection that carries the new bearer.
  host.approve("new");
  await until(() => connection() === "live");
  expect(sessionView()).not.toBeNull();
  expect(signInView()).toBeNull();
  expect(
    sentWith("new").filter((request) => request.path.includes("/v1/events")),
  ).toHaveLength(1);
});

test("a session's catch-up read refused at the first request shows the sign-in view", async () => {
  await bearer.store("revoked");
  await mount("/sessions/work/");
  await until(() => signInView() !== null);
  expect(sessionView()).toBeNull();
  expect(bearer.held()).toBeNull();
  const sent = sentWith("revoked").length;
  await settle(60_000);
  expect(sentWith("revoked")).toHaveLength(sent);
});

test("a 401 to the archived-sessions read shows the sign-in view, not its error", async () => {
  host.valid.add("old");
  await bearer.store("old");
  host.force = (path) => (path.includes("cwd=") ? 401 : null);
  await mount("/?resume=picker&cwd=%2Fwork");
  await until(() => signInView() !== null);

  expect(element.querySelector(".resume-page")).toBeNull();
  expect(element.textContent).not.toContain("Loading archived sessions");
  expect(element.textContent).not.toContain("Authorization");
  expect(bearer.held()).toBeNull();
});

test("a 401 to a file read shows the sign-in view, not the viewer's refusal", async () => {
  host.valid.add("old");
  await bearer.store("old");
  host.force = (path) => (path.includes("/files/") ? 401 : null);
  await mount("/sessions/work/tree");
  await until(() => signInView() !== null);

  expect(element.textContent).not.toContain("Authorization");
  expect(bearer.held()).toBeNull();
  expect(host.open()).toBe(0);
});

test("another failure of the archived-sessions read stays in its view as the error", async () => {
  host.valid.add("old");
  await bearer.store("old");
  host.force = (path) => (path.includes("cwd=") ? 500 : null);
  await mount("/?resume=picker&cwd=%2Fwork");
  await until(() => element.querySelector(".resume-error") !== null);

  expect(element.querySelector(".resume-error")?.textContent).toContain(
    "Loading archived sessions",
  );
  expect(signInView()).toBeNull();
  expect(bearer.held()).toBe("old");
});

test("a probe answered after a newer bearer was stored is dropped, and its 401 leaves the new bearer held", async () => {
  await bearer.store("old");
  host.hold((token) => token === "old");
  await mount("/");

  // A sign-in lands while the probe under the old bearer is out.
  host.valid.add("new");
  await act(async () => bearer.store("new"));
  await until(() => listView() !== null);

  host.release();
  await settle();
  expect(listView()).not.toBeNull();
  expect(signInView()).toBeNull();
  expect(bearer.held()).toBe("new");
});

test("with no bearer held the sign-in view is shown from the first frame, and storing one brings in the list", async () => {
  await mount("/");
  expect(signInView()).not.toBeNull();
  expect(host.seen.every((request) => request.bearer === null)).toBe(true);

  host.valid.add("new");
  await act(async () => bearer.store("new"));
  await until(() => listView() !== null);
  expect(signInView()).toBeNull();
});

test("a held bearer the host refuses at the first request is dropped, and the sign-in view is shown", async () => {
  await bearer.store("stale");
  await mount("/");
  await until(() => signInView() !== null);
  expect(bearer.held()).toBeNull();
  expect(listView()).toBeNull();

  host.approve("fresh");
  await until(() => listView() !== null);
  expect(bearer.held()).toBe("fresh");
});
