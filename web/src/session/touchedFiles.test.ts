import { describe, expect, it } from "vitest";
import type { LogEntry } from "../api/types.ts";
import {
  anchorForCall,
  editedPath,
  fileActivity,
  lastEdit,
  openablePaths,
  viewablePath,
} from "./touchedFiles.ts";

describe("viewablePath", () => {
  it("passes relative paths through, normalized", () => {
    expect(viewablePath("src/api.rs")).toBe("src/api.rs");
    expect(viewablePath("./src/./api.rs")).toBe("src/api.rs");
  });

  it("declines absolute paths — the surface is within-cwd only", () => {
    expect(viewablePath("/etc/passwd")).toBeNull();
    expect(viewablePath("/Users/jbr/code/efference/src/api.rs")).toBeNull();
  });

  it("declines root-escaping and empty paths", () => {
    expect(viewablePath("../sibling/file")).toBeNull();
    expect(viewablePath("")).toBeNull();
    expect(viewablePath("./")).toBeNull();
  });
});

describe("editedPath", () => {
  it("reads write's `path` and the edit tools' `file_path`", () => {
    expect(editedPath("write", { path: "a.md", contents: "x" })).toBe("a.md");
    expect(editedPath("edit", { file_path: "b.rs" })).toBe("b.rs");
    expect(editedPath("undo_edit", { file_path: "b.rs" })).toBe("b.rs");
  });

  it("is null for reads and unknown tools — reads do not move the view", () => {
    expect(editedPath("read", { paths: ["a.md"] })).toBeNull();
    expect(editedPath("bash", { command: "touch a.md" })).toBeNull();
  });

  it("is null for an absolute target", () => {
    expect(editedPath("write", { path: "/tmp/out" })).toBeNull();
  });
});

describe("openablePaths", () => {
  it("includes reads, filtered to viewable ones", () => {
    expect(
      openablePaths("read", { paths: ["a.md", "/etc/passwd", "b.rs"] }),
    ).toEqual(["a.md", "b.rs"]);
  });

  it("includes the edited file", () => {
    expect(openablePaths("write", { path: "a.md" })).toEqual(["a.md"]);
  });

  it("is empty for tools naming no file", () => {
    expect(openablePaths("bash", { command: "ls" })).toEqual([]);
  });
});

let seq = 0;
const entry = (payload: LogEntry["payload"]): LogEntry => ({
  seq: ++seq,
  turn: 1,
  at: 0,
  payload,
});
const assistant = (
  calls: { id: string; name: string; input: unknown }[],
): LogEntry => entry({ Assistant: { thinking: [], text: [], calls } });
const result = (callId: string, isError = false, content = ""): LogEntry =>
  entry({
    ToolResult: {
      call_id: callId,
      raw: { content, is_error: isError },
      wire: "Raw",
    },
  });

/** An edit result carrying the `===DIFF===` payload the anchor comes from. */
const diffResult = (callId: string, added: string) =>
  result(callId, false, `edited\n\n===DIFF===\n\n ctx\n+${added}\n ctx2`);

describe("lastEdit", () => {
  it("finds the newest committed edit's path and seq", () => {
    const entries = [
      assistant([{ id: "c1", name: "write", input: { path: "a.md" } }]),
      result("c1"),
      assistant([{ id: "c2", name: "edit", input: { file_path: "b.rs" } }]),
      result("c2"),
    ];
    const last = lastEdit(entries);
    expect(last?.path).toBe("b.rs");
    expect(last?.seq).toBe(entries[3].seq);
  });

  it("skips errored results — nothing changed on disk", () => {
    const entries = [
      assistant([{ id: "c1", name: "write", input: { path: "a.md" } }]),
      result("c1"),
      assistant([{ id: "c2", name: "edit", input: { file_path: "b.rs" } }]),
      result("c2", true),
    ];
    expect(lastEdit(entries)?.path).toBe("a.md");
  });

  it("skips reads and calls without results", () => {
    const entries = [
      assistant([{ id: "c1", name: "read", input: { paths: ["a.md"] } }]),
      result("c1"),
      assistant([{ id: "c2", name: "write", input: { path: "b.md" } }]),
    ];
    expect(lastEdit(entries)).toBeNull();
  });

  it("skips out-of-project edits", () => {
    const entries = [
      assistant([{ id: "c1", name: "write", input: { path: "/tmp/x" } }]),
      result("c1"),
    ];
    expect(lastEdit(entries)).toBeNull();
  });
});

describe("lastEdit anchors", () => {
  it("carries the edit's own diff anchor, so follow can land on it", () => {
    const entries = [
      assistant([{ id: "c1", name: "edit", input: { file_path: "b.rs" } }]),
      diffResult("c1", "    fresh();"),
    ];
    expect(lastEdit(entries)?.anchor).toEqual({
      added: ["    fresh();"],
      before: ["ctx"],
      after: ["ctx2"],
    });
  });

  it("is null when the result carried no diff", () => {
    const entries = [
      assistant([{ id: "c1", name: "write", input: { path: "a.md" } }]),
      result("c1"),
    ];
    expect(lastEdit(entries)?.anchor).toBeNull();
  });
});

describe("anchorForCall", () => {
  it("finds a historical call's anchor, not just the newest", () => {
    const entries = [
      assistant([{ id: "c1", name: "edit", input: { file_path: "b.rs" } }]),
      diffResult("c1", "    early();"),
      assistant([{ id: "c2", name: "edit", input: { file_path: "b.rs" } }]),
      diffResult("c2", "    later();"),
    ];
    expect(anchorForCall(entries, "c1")?.added).toEqual(["    early();"]);
  });

  it("is null for an errored call, a missing one, and no call at all", () => {
    const entries = [
      assistant([{ id: "c1", name: "edit", input: { file_path: "b.rs" } }]),
      result("c1", true, "boom\n\n===DIFF===\n\n+x"),
    ];
    expect(anchorForCall(entries, "c1")).toBeNull();
    expect(anchorForCall(entries, "nope")).toBeNull();
    expect(anchorForCall(entries, null)).toBeNull();
  });
});

describe("fileActivity", () => {
  it("separates what was read from what was changed", () => {
    const entries = [
      assistant([
        { id: "c1", name: "read", input: { paths: ["a.md", "b.rs"] } },
      ]),
      result("c1"),
      assistant([{ id: "c2", name: "edit", input: { file_path: "b.rs" } }]),
      result("c2"),
    ];
    const activity = fileActivity(entries);
    expect(activity.get("a.md")).toMatchObject({ read: true, edited: false });
    expect(activity.get("b.rs")).toMatchObject({ read: true, edited: true });
  });

  it("counts nothing an errored call touched", () => {
    const entries = [
      assistant([{ id: "c1", name: "read", input: { paths: ["a.md"] } }]),
      result("c1", true),
    ];
    expect(fileActivity(entries).size).toBe(0);
  });
});
