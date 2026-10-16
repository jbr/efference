// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { Composer } from "./Composer.tsx";
import { acquireDraft } from "./drafts.ts";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
beforeEach(() => {
  const values = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => void values.set(key, value),
    removeItem: (key: string) => void values.delete(key),
  });
});
afterEach(() => vi.unstubAllGlobals());

const defaults = {
  recalled: null,
  pace: null,
  phase: null,
  verbs: null,
  stall: null,
  retrying: false,
  gatePending: false,
  ask: null,
  cold: null,
  compactionFailure: null,
  operations: null,
  model: "test",
};
function mount(send = vi.fn(() => Promise.resolve())) {
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const render = (id: string) => {
    history.replaceState(null, "", `/sessions/${id}/`);
    act(() => root.render(<Composer key={id} {...defaults} submit={send} />));
  };
  const close = () => {
    act(() => root.unmount());
    host.remove();
  };
  return { host, render, close };
}

test("composer text and completed attachments return after switching sessions", () => {
  const a = `draft-${crypto.randomUUID()}`;
  const b = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${a}`);
  store.setText("a's unsent text");
  store.restore([
    {
      name: "note.txt",
      path: "/stored/note.txt",
      kind: { Binary: { bytes: 5 } },
    },
  ]);
  const view = mount();
  try {
    view.render(a);
    expect(view.host.querySelector("textarea")?.value).toBe("a's unsent text");
    expect(view.host.querySelector(".attachment-name")?.textContent).toBe(
      "note.txt",
    );
    view.render(b);
    expect(view.host.querySelector("textarea")?.value).toBe("");
    expect(view.host.querySelector(".attachments")).toBeNull();
    view.render(a);
    expect(view.host.querySelector("textarea")?.value).toBe("a's unsent text");
    expect(view.host.querySelector(".attachment-name")?.textContent).toBe(
      "note.txt",
    );
  } finally {
    view.close();
  }
});

test("failed sends retain a draft and successful sends clear it", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${id}`);
  store.setText("keep until accepted");
  const send = vi
    .fn()
    .mockRejectedValueOnce(new Error("offline"))
    .mockResolvedValueOnce(undefined);
  const view = mount(send);
  try {
    view.render(id);
    await act(async () => {
      view.host
        .querySelector("form")
        ?.dispatchEvent(
          new Event("submit", { bubbles: true, cancelable: true }),
        );
    });
    expect(store.snapshot().text).toBe("keep until accepted");
    await act(async () => {
      view.host
        .querySelector("form")
        ?.dispatchEvent(
          new Event("submit", { bubbles: true, cancelable: true }),
        );
    });
    expect(store.snapshot().text).toBe("");
    expect(view.host.querySelector("textarea")?.value).toBe("");
  } finally {
    view.close();
  }
});

test("a send accepted after switching does not clear either session's newer draft", async () => {
  const a = `draft-${crypto.randomUUID()}`;
  const b = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${a}`);
  store.setText("first turn");
  let resolve!: () => void;
  const pending = new Promise<void>((yes) => {
    resolve = yes;
  });
  const send = vi.fn(() => pending);
  const view = mount(send);
  try {
    view.render(a);
    act(() => {
      view.host
        .querySelector("form")
        ?.dispatchEvent(
          new Event("submit", { bubbles: true, cancelable: true }),
        );
    });
    act(() => store.setText("next turn"));
    view.render(b);
    act(() => acquireDraft(`/sessions/${b}`).setText("b draft"));
    await act(async () => {
      resolve();
      await pending;
    });
    expect(store.snapshot().text).toBe("next turn");
    expect(view.host.querySelector("textarea")?.value).toBe("b draft");
    view.render(a);
    expect(view.host.querySelector("textarea")?.value).toBe("next turn");
  } finally {
    view.close();
  }
});

test("composer hold sends a loop patch without submitting or clearing its draft", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const store = acquireDraft(`/sessions/${id}`);
  store.setText("keep this draft while holding");
  const fetch = vi.fn(
    async () =>
      new Response(
        JSON.stringify({
          pace: { held: true, speed_ms: 0 },
          review_level: "spotter_human",
          loop_phase: { phase: "idle" },
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      ),
  );
  vi.stubGlobal("fetch", fetch);
  const send = vi.fn(() => Promise.resolve());
  const view = mount(send);
  try {
    view.render(id);
    await act(async () =>
      view.host.querySelector<HTMLButtonElement>(".hold-btn")!.click(),
    );
    expect(fetch).toHaveBeenCalledOnce();
    const [url, init] = fetch.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe(`/sessions/${id}/v1/loop`);
    expect(init.method).toBe("PATCH");
    expect(JSON.parse(init.body as string)).toEqual({ pace: { held: true } });
    expect(send).not.toHaveBeenCalled();
    expect(store.snapshot().text).toBe("keep this draft while holding");
  } finally {
    view.close();
  }
});

test("an empty send the verbs topic refuses states the reason without a request", async () => {
  const id = `draft-${crypto.randomUUID()}`;
  const fetch = vi.fn();
  vi.stubGlobal("fetch", fetch);
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  history.replaceState(null, "", `/sessions/${id}/`);
  const verbs = {
    turn: { accepted: true, delivery: "now" },
    continue: {
      accepted: false,
      message: "nothing to continue: the last turn completed",
    },
    compact: { accepted: true, warm: true },
  } as const;
  try {
    act(() =>
      root.render(
        <Composer
          key={id}
          {...defaults}
          phase={{ phase: "idle" }}
          verbs={verbs}
          submit={vi.fn(() => Promise.resolve())}
        />,
      ),
    );
    await act(async () =>
      host.querySelector<HTMLFormElement>("form")!.requestSubmit(),
    );
    expect(fetch).not.toHaveBeenCalled();
    expect(host.querySelector(".error")?.textContent).toBe(
      "nothing to continue: the last turn completed",
    );
  } finally {
    act(() => root.unmount());
    host.remove();
  }
});
