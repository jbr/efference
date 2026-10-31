import { expect, it } from "vitest";
import type { StartingView } from "../api/types.ts";
import { startingLine } from "./startingLine.ts";

const starting = (patch: Partial<StartingView>): StartingView => ({
  id: "fresh",
  cwd: "/work",
  since: 1,
  resume: false,
  stage: "environment",
  authorizing: null,
  failure: null,
  ...patch,
});

it("names the stage, and a server awaiting authorization over it", () => {
  expect(startingLine(starting({}))).toBe("Starting — running the env hook");
  expect(startingLine(starting({ stage: "preparing" }))).toBe(
    "Starting — loading the config and mounting MCP servers",
  );
  expect(
    startingLine(starting({ stage: "preparing", authorizing: "exa" })),
  ).toBe(
    "Starting — waiting for exa to be authorized in a browser on the host",
  );
});
