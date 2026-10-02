import { describe, expect, it } from "vitest";
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
