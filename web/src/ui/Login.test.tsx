// @vitest-environment jsdom
// The sign-in view keeps what the mint answers (docs/web-bind.md): the token
// is held before the connect re-runs, so the first request after sign-in
// already carries it, and a refused code leaves nothing held. The trade names
// the browser, for the host's device listing.
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, test, vi } from "vitest";
import { bearer } from "../api/bearer.ts";
import { deviceLabel } from "./deviceLabel.ts";
import { Login } from "./Login.tsx";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

afterEach(async () => {
  vi.unstubAllGlobals();
  await bearer.clear();
});

/** Render the view with no boot fragment, type `code`, and submit it. The
 * host is attached: a form outside the document does not submit. */
async function submit(code: string, onAuthenticated: () => void) {
  const host = document.body.appendChild(document.createElement("div"));
  const root = createRoot(host);
  await act(async () =>
    root.render(<Login onAuthenticated={onAuthenticated} />),
  );
  const input = host.querySelector("input")!;
  // React tracks an input's value through the prototype's setter.
  const setValue = Object.getOwnPropertyDescriptor(
    HTMLInputElement.prototype,
    "value",
  )!.set!;
  act(() => {
    setValue.call(input, code);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await act(async () => host.querySelector("button")!.click());
  return {
    host,
    unmount: () => {
      act(() => root.unmount());
      host.remove();
    },
  };
}

test("a code the mint accepts is traded for a token, held before the connect re-runs", async () => {
  const fetchMock = vi
    .fn()
    .mockResolvedValue(Response.json({ token: "minted" }, { status: 200 }));
  vi.stubGlobal("fetch", fetchMock);
  const heldAtConnect: (string | null)[] = [];

  const { unmount } = await submit("123456", () =>
    heldAtConnect.push(bearer.held()),
  );
  try {
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/auth/session");
    expect(JSON.parse(init.body as string)).toEqual({
      code: "123456",
      label: deviceLabel(navigator.userAgent),
    });
    expect(heldAtConnect).toEqual(["minted"]);
  } finally {
    unmount();
  }
});

test("a refused code shows the daemon's reason and holds nothing", async () => {
  vi.stubGlobal(
    "fetch",
    vi
      .fn()
      .mockResolvedValue(
        Response.json(
          { code: "unauthorized", message: "code already used" },
          { status: 401 },
        ),
      ),
  );
  const onAuthenticated = vi.fn();

  const { host, unmount } = await submit("123456", onAuthenticated);
  try {
    expect(host.textContent).toContain("code already used");
    expect(onAuthenticated).not.toHaveBeenCalled();
    expect(bearer.held()).toBeNull();
  } finally {
    unmount();
  }
});
