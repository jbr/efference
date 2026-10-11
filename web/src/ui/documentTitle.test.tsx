// @vitest-environment jsdom
import { act, StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { expect, test } from "vitest";
import { rememberListing } from "../session/listing.ts";
import {
  APP_TITLE,
  sessionDocumentTitle,
  useDocumentTitle,
} from "./documentTitle.ts";
import { sessionName } from "./identity.ts";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

function View({ name }: { name: string | null }) {
  useDocumentTitle(sessionDocumentTitle(name));
  return null;
}
function Page({ id, name }: { id: string | null; name: string | null }) {
  useDocumentTitle(id === null ? APP_TITLE : null);
  return id === null ? null : <View key={id} name={name} />;
}

test("title follows active views and renames without retaining an unmounted title", () => {
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
  } finally {
    act(() => root.unmount());
    host.remove();
  }
  expect(document.title).toBe(APP_TITLE);
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
