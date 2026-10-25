// @vitest-environment jsdom
// The sign-in view (docs/web-bind.md): a bare visit asks the host for a code
// under the browser's label, shows it with the command that approves it, and
// asks every second until the token comes back, which is held before the
// connect re-runs; an expired code is said so with a new one offered; and a
// link's claim, from the boot fragment or a later hashchange, is claimed at once.
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { bearer } from "../api/bearer.ts";
import { deviceLabel } from "./deviceLabel.ts";
import { Login, POLL_MS } from "./Login.tsx";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const label = deviceLabel(navigator.userAgent);

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(async () => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  history.replaceState(null, "", "/");
  await bearer.clear();
});

const asked = (claim: string, code: string) =>
  Response.json({ claim, code, expires_in: 90 }, { status: 200 });
const pending = () => Response.json({ status: "pending" }, { status: 202 });
const minted = (token: string) => Response.json({ token }, { status: 200 });
const unknown = () =>
  Response.json(
    { code: "not_found", message: "no pending sign-in answers to that claim" },
    { status: 404 },
  );

interface Call {
  url: string;
  body: unknown;
}

/** A host that answers `/pair` from a queue and `/pair/claim` from a queue or
 * by the claim presented, the last answer in a queue repeating, and records every
 * request. */
function host(script: {
  pair?: Response[];
  claim?: Response[] | ((claim: string) => Response);
}) {
  const calls: Call[] = [];
  const pairs = script.pair ?? [];
  const claims = script.claim ?? [];
  const next = (queue: Response[]) =>
    queue.length > 1 ? queue.shift()! : queue[0];
  vi.stubGlobal(
    "fetch",
    vi.fn((url: string, init?: RequestInit) => {
      const body = JSON.parse(String(init?.body));
      calls.push({ url: String(url), body });
      const answer =
        String(url) === "/pair"
          ? next(pairs)
          : typeof claims === "function"
            ? claims(body.claim)
            : next(claims);
      return Promise.resolve(answer.clone());
    }),
  );
  return {
    calls,
    claims: () => calls.filter((call) => call.url === "/pair/claim"),
    pairs: () => calls.filter((call) => call.url === "/pair"),
  };
}

/** Let the requests in flight answer and React render what they changed. */
async function settle(ms = 0) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

async function render(view: typeof Login = Login) {
  const onAuthenticated = vi.fn();
  const element = document.body.appendChild(document.createElement("div"));
  const root = createRoot(element);
  const View = view;
  await act(async () =>
    root.render(<View onAuthenticated={onAuthenticated} />),
  );
  await settle();
  return {
    element,
    onAuthenticated,
    unmount: () => {
      act(() => root.unmount());
      element.remove();
    },
  };
}

test("a bare visit asks under the browser's label and shows the code with the command that approves it", async () => {
  const server = host({ pair: [asked("c1", "KQ7-M4X")], claim: [pending()] });
  const { element, unmount } = await render();
  try {
    expect(server.pairs()).toEqual([{ url: "/pair", body: { label } }]);
    expect(element.querySelector(".login-code")?.textContent).toBe("KQ7-M4X");
    expect(element.textContent).toContain("efference devices approve");
    expect(element.textContent).toContain("expires in 1:30");
    expect(element.textContent).toContain(label);
    expect(element.querySelector("input")).toBeNull();
    await settle(10_000);
    expect(element.textContent).toContain("expires in 1:20");
  } finally {
    unmount();
  }
});

test("it asks every second, and holds the token before the connect re-runs", async () => {
  const server = host({
    pair: [asked("c1", "KQ7-M4X")],
    claim: [pending(), pending(), minted("minted")],
  });
  const heldAtConnect: (string | null)[] = [];
  const { onAuthenticated, unmount } = await render();
  onAuthenticated.mockImplementation(() => heldAtConnect.push(bearer.held()));
  try {
    expect(server.claims()).toHaveLength(1);
    await settle(POLL_MS);
    expect(server.claims()).toHaveLength(2);
    expect(onAuthenticated).not.toHaveBeenCalled();
    await settle(POLL_MS);
    expect(server.claims()).toHaveLength(3);
    expect(server.claims()[0].body).toEqual({ claim: "c1", label });
    expect(heldAtConnect).toEqual(["minted"]);
    await settle(5 * POLL_MS);
    expect(server.claims()).toHaveLength(3);
  } finally {
    unmount();
  }
});

test("an expired code says so and offers a new one", async () => {
  const server = host({
    pair: [asked("c1", "KQ7-M4X"), asked("c2", "ZZZ-222")],
    claim: [unknown(), pending()],
  });
  const { element, onAuthenticated, unmount } = await render();
  try {
    expect(element.textContent).toContain("That code expired");
    expect(element.querySelector(".login-code")).toBeNull();
    await settle(5 * POLL_MS);
    expect(server.claims()).toHaveLength(1);

    const button = element.querySelector("button")!;
    expect(button.textContent).toBe("new code");
    await act(async () => button.click());
    await settle();
    expect(server.pairs()).toHaveLength(2);
    expect(element.querySelector(".login-code")?.textContent).toBe("ZZZ-222");
    expect(server.claims().at(-1)?.body).toEqual({ claim: "c2", label });
    expect(onAuthenticated).not.toHaveBeenCalled();
    expect(bearer.held()).toBeNull();
  } finally {
    unmount();
  }
});

test("a failure that passes is shown while it lasts, and the asking goes on", async () => {
  const server = host({
    pair: [asked("c1", "KQ7-M4X")],
    claim: [
      Response.json(
        { code: "internal", message: "could not read the pending sign-ins" },
        { status: 500 },
      ),
      pending(),
      minted("minted"),
    ],
  });
  const { element, onAuthenticated, unmount } = await render();
  try {
    expect(element.textContent).toContain(
      "could not read the pending sign-ins",
    );
    expect(element.querySelector(".login-code")?.textContent).toBe("KQ7-M4X");
    await settle(POLL_MS);
    expect(element.textContent).not.toContain("could not read");
    await settle(POLL_MS);
    expect(server.claims()).toHaveLength(3);
    expect(onAuthenticated).toHaveBeenCalledOnce();
    expect(bearer.held()).toBe("minted");
  } finally {
    unmount();
  }
});

test("a code the host stops answering for is given up on once its life is over", async () => {
  const server = host({ pair: [asked("c1", "KQ7-M4X")], claim: [pending()] });
  const { element, unmount } = await render();
  try {
    await settle(100_000);
    expect(element.textContent).toContain("That code expired");
    const asked = server.claims().length;
    await settle(10 * POLL_MS);
    expect(server.claims()).toHaveLength(asked);
  } finally {
    unmount();
  }
});

test("a link's claim in the boot fragment is claimed at once, and the fragment scrubbed", async () => {
  history.replaceState(null, "", "/sessions/work/#claim=from-the-link");
  vi.resetModules();
  // Fresh modules, so the boot read sees this fragment; the bearer is the one
  // those modules share.
  const { Login: Booted } = await import("./Login.tsx");
  const { bearer: booted } = await import("../api/bearer.ts");
  expect(location.hash).toBe("");
  expect(location.pathname).toBe("/sessions/work/");
  const server = host({ claim: [minted("from-link")] });
  const { onAuthenticated, unmount } = await render(Booted);
  try {
    expect(server.pairs()).toEqual([]);
    expect(server.claims()).toEqual([
      { url: "/pair/claim", body: { claim: "from-the-link", label } },
    ]);
    expect(onAuthenticated).toHaveBeenCalledOnce();
    expect(booted.held()).toBe("from-link");
  } finally {
    unmount();
    await booted.clear();
  }
});

test("a link opened into a waiting view is claimed, and the code's asking stops", async () => {
  const server = host({
    pair: [asked("c1", "KQ7-M4X")],
    claim: [pending()],
  });
  const { onAuthenticated, unmount } = await render();
  try {
    expect(server.claims()).toHaveLength(1);
    const opened = vi.fn().mockResolvedValue(minted("from-dev-open"));
    vi.stubGlobal("fetch", opened);
    history.replaceState(null, "", "/#claim=opened-later");
    await act(async () => {
      window.dispatchEvent(new HashChangeEvent("hashchange"));
    });
    await settle();
    expect(location.hash).toBe("");
    expect(opened).toHaveBeenCalledOnce();
    expect(JSON.parse(String(opened.mock.calls[0][1]?.body))).toEqual({
      claim: "opened-later",
      label,
    });
    expect(bearer.held()).toBe("from-dev-open");
    expect(onAuthenticated).toHaveBeenCalledOnce();
    await settle(5 * POLL_MS);
    expect(opened).toHaveBeenCalledOnce();
  } finally {
    unmount();
  }
});

test("a link that names nothing falls back to a code", async () => {
  const server = host({
    pair: [asked("c1", "KQ7-M4X")],
    claim: (claim) => (claim === "already-used" ? unknown() : pending()),
  });
  const { element, unmount } = await render();
  try {
    // The bare render asked for a code; a dead link arriving later says so and
    // asks for another.
    history.replaceState(null, "", "/#claim=already-used");
    await act(async () => {
      window.dispatchEvent(new HashChangeEvent("hashchange"));
    });
    await settle();
    expect(server.claims().map((call) => call.body)).toContainEqual({
      claim: "already-used",
      label,
    });
    expect(element.textContent).toContain(
      "That sign-in link was used already or has expired.",
    );
    expect(server.pairs()).toHaveLength(2);
    expect(element.querySelector(".login-code")?.textContent).toBe("KQ7-M4X");
  } finally {
    unmount();
  }
});

test("a host that refuses the request is shown, with a retry", async () => {
  host({
    pair: [
      Response.json(
        {
          code: "forbidden",
          message: 'host "evil.example" is not this daemon\'s name',
        },
        { status: 403 },
      ),
      asked("c1", "KQ7-M4X"),
    ],
    claim: [pending()],
  });
  const { element, unmount } = await render();
  try {
    expect(element.textContent).toContain("is not this daemon's name");
    const button = element.querySelector("button")!;
    expect(button.textContent).toBe("try again");
    await act(async () => button.click());
    await settle();
    expect(element.querySelector(".login-code")?.textContent).toBe("KQ7-M4X");
  } finally {
    unmount();
  }
});
