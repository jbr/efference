import { afterEach, expect, test, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { SessionList } from "./SessionList.tsx";

vi.mock("./NewSessionForm.tsx", () => ({
  NewSessionForm: ({
    requested,
    page,
  }: {
    requested: string | null;
    page: string;
  }) => <div data-requested={requested} data-page={page} />,
}));
afterEach(() => vi.unstubAllGlobals());

test("a session's project query initializes requested on the settings page", () => {
  vi.stubGlobal("location", {
    search: "?new=settings&cwd=%2Fwork%2Fa+%26+b",
  });
  const markup = renderToStaticMarkup(<SessionList sessions={[]} />);
  expect(markup).toContain('data-requested="/work/a &amp; b"');
  expect(markup).toContain('data-page="settings"');
});

test("a launch without cwd leaves location selection to the form", () => {
  vi.stubGlobal("location", { search: "?new=settings" });
  const markup = renderToStaticMarkup(<SessionList sessions={[]} />);
  expect(markup).not.toContain("data-requested");
  expect(markup).toContain('data-page="settings"');
});
