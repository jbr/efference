import { describe, expect, it, test } from "vitest";
import { launchPage, launchUrl } from "./newSessionNavigation.ts";

describe("launch routes", () => {
  it("identifies the two stages and each model role", () => {
    expect(launchPage("?new=location")).toBe("location");
    expect(launchPage("?new=settings")).toBe("settings");
    for (const role of ["main", "spotter", "triage"] as const) {
      expect(launchPage(`?new=model&role=${role}`)).toBe(role);
      expect(launchPage(launchUrl("https://host/", role).slice(1))).toBe(role);
    }
  });
  it("does not mistake invalid roles or unrelated queries for launch routes", () => {
    for (const search of [
      "",
      "?new=model",
      "?new=model&role=unknown",
      "?new=unknown",
      "?role=main",
    ]) {
      expect(launchPage(search)).toBeNull();
    }
  });
  it("changes only launch parameters, preserving the path, other queries, and hash", () => {
    const url = "https://host/?view=all&new=model&role=spotter#anchor";
    expect(launchUrl(url, "settings")).toBe("/?view=all&new=settings#anchor");
    expect(launchUrl(url, "main")).toBe(
      "/?view=all&new=model&role=main#anchor",
    );
    expect(launchUrl(url, null)).toBe("/?view=all#anchor");
  });
});

test("project launch URLs round-trip cwd through settings and model selection", () => {
  const cwd = "/work/a & b/#é";
  const settings = launchUrl("/", "settings", cwd);
  const url = new URL(settings, "https://host");
  expect(launchPage(url.search)).toBe("settings");
  expect(url.searchParams.get("cwd")).toBe(cwd);
  const model = launchUrl(url.href, "main");
  expect(new URL(model, url).searchParams.get("cwd")).toBe(cwd);
  expect(launchUrl(new URL(model, url).href, null)).toBe("/");
});
