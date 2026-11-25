// @vitest-environment jsdom
import { act, StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, test, vi } from "vitest";
import { rememberListing } from "../session/listing.ts";
import {
  APP_TITLE,
  sessionDocumentTitle,
  useDocumentTitle,
} from "./documentTitle.ts";
import { sessionName } from "./identity.ts";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

afterEach(() => vi.unstubAllGlobals());

function View({ name }: { name: string | null }) {
  useDocumentTitle(sessionDocumentTitle(name));
  return null;
}
function Page({ id, name }: { id: string | null; name: string | null }) {
  useDocumentTitle(id === null ? APP_TITLE : null);
  return id === null ? null : <View key={id} name={name} />;
}

/** Render the title-bearing views against the current display mode, handing
 * each render to `check`; the host and root are torn down, and the title
 * restored, whatever `check` does. */
function withTitlePage(
  check: (render: (id: string | null, name: string | null) => void) => void,
) {
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const render = (id: string | null, name: string | null) =>
    act(() =>
      root.render(
        <StrictMode>
          <Page id={id} name={name} />
        </StrictMode>,
      ),
    );
  try {
    check(render);
  } finally {
    act(() => root.unmount());
    host.remove();
  }
  expect(document.title).toBe(APP_TITLE);
}

/** The installed-app detection `sessionDocumentTitle` reads. */
function stubDisplay(standalone: boolean) {
  vi.stubGlobal("matchMedia", () => ({ matches: standalone }));
  vi.stubGlobal("navigator", { standalone: false });
}

test("title follows active views and renames without retaining an unmounted title", () => {
  stubDisplay(false);
  withTitlePage((render) => {
    render("first", "First task");
    expect(document.title).toBe("First task · efference");
    render("first", "Renamed task");
    expect(document.title).toBe("Renamed task · efference");
    render("second", "second");
    expect(document.title).toBe("second · efference");
    render("root-session", null);
    expect(document.title).toBe(APP_TITLE);
    render("first", "First task");
    render(null, null);
    expect(document.title).toBe(APP_TITLE);
    render("second", "Second task");
  });
});

test("the installed app shows the bare session name through rename and app title", () => {
  stubDisplay(true);
  withTitlePage((render) => {
    render("first", "First task");
    expect(document.title).toBe("First task");
    render("first", "Renamed task");
    expect(document.title).toBe("Renamed task");
    render(null, null);
    expect(document.title).toBe(APP_TITLE);
  });
});

test("the title names the app once in an installed app and beside the session in a tab", () => {
  stubDisplay(true);
  expect(sessionDocumentTitle("Diagnosing CI")).toBe("Diagnosing CI");
  expect(sessionDocumentTitle(null)).toBe(APP_TITLE);

  vi.stubGlobal("matchMedia", () => ({ matches: false }));
  expect(sessionDocumentTitle("Diagnosing CI")).toBe(
    "Diagnosing CI · efference",
  );
  expect(sessionDocumentTitle(null)).toBe(APP_TITLE);
});

test("iOS standalone counts as the installed app", () => {
  vi.stubGlobal("matchMedia", () => ({ matches: false }));
  vi.stubGlobal("navigator", { standalone: true });
  expect(sessionDocumentTitle("Task")).toBe("Task");
});

test("loading and untitled sessions use the same name fallback as the header", () => {
  rememberListing([]);
  expect(sessionName(null, "loading-session")).toBe("loading-session");
  expect(sessionName(null, null)).toBeNull();
  expect(
    sessionName({ id: "untitled", cwd: "/", model: "test" }, "untitled"),
  ).toBe("untitled");
  expect(
    sessionName(
      { id: "named", title: "Current title", cwd: "/", model: "test" },
      "named",
    ),
  ).toBe("Current title");
});
