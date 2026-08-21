import { describe, expect, it } from "vitest";
import type { LogEntry } from "../api/types.ts";
import {
  editedPath,
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
const result = (callId: string, isError = false): LogEntry =>
  entry({
    ToolResult: {
      call_id: callId,
      raw: { content: "", is_error: isError },
      wire: "Raw",
    },
  });

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
