// The chip table and what it renders (docs/web-client.md "An argument can carry
// a chip"). The presentational half is rendered to static markup — no layout is
// measured, only what each chip says and which element it is — and the fetch
// half is exercised through the module's own loader with a stubbed fetch.

import { afterEach, expect, test, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import type { ProcessRecord } from "../api/types.ts";
import { CallHead } from "./CallHead.tsx";
import { callView } from "./signature.ts";
import { layoutSignature } from "./signatureLayout.ts";
import { argChip, chipCommand } from "./chipTable.ts";
import { ArgValue, ProcessChip } from "./chips.tsx";
import { loadProcessRecord } from "./processRecord.ts";

const record = (over: Partial<ProcessRecord> = {}): ProcessRecord => ({
  id: 4,
  command: "cargo run --release --bin efference",
  spawned_at: 1754300000000,
  background: true,
  muted: false,
  running: true,
  ...over,
});

const json = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), { status });

afterEach(() => vi.unstubAllGlobals());

test("the table upgrades handles and paths, and nothing else", () => {
  expect(argChip("kill_process", "handle", "p#4")).toEqual({
    kind: "process",
    id: 4,
  });
  expect(argChip("read_process", "handle", "p#12")).toEqual({
    kind: "process",
    id: 12,
  });
  expect(argChip("watch_process", "handle", "p#0")).toEqual({
    kind: "process",
    id: 0,
  });
  expect(argChip("write", "path", "src/api.rs")).toEqual({
    kind: "path",
    path: "src/api.rs",
  });
  expect(argChip("edit", "file_path", "src/api.rs")).toEqual({
    kind: "path",
    path: "src/api.rs",
  });
  // Outside the viewer's surface — absolute, or escaping the root — is no chip
  // at all (session/touchedFiles.ts).
  expect(argChip("write", "path", "/etc/passwd")).toBeNull();
  expect(argChip("edit", "file_path", "../elsewhere.rs")).toBeNull();
  // A handle that is not one.
  expect(argChip("kill_process", "handle", "the build")).toBeNull();
});

test("search, read and list arguments carry no chip", () => {
  // The discipline: their arguments already say everything, so annotating them
  // could only re-word them.
  expect(argChip("search", "path", "src")).toBeNull();
  expect(argChip("search", "pattern", "Listening")).toBeNull();
  expect(argChip("read", "paths", ["src/api.rs"])).toBeNull();
  expect(argChip("read", "path", "src/api.rs")).toBeNull();
  expect(argChip("list", "path", "src")).toBeNull();
  // And a mounted server's same-named tool is not the built-in.
  expect(argChip("fs__write", "path", "src/api.rs")).toBeNull();
});

test("a resolved handle chip states the command it stood for", () => {
  const markup = renderToStaticMarkup(
    <ProcessChip record={record()} text='"p#4"' />,
  );
  expect(markup).toContain("p#4");
  expect(markup).toContain("cargo run --release");
  expect(markup).toContain("process-chip running");
  // The whole command is the title, whatever the chip shows of it.
  expect(markup).toContain("cargo run --release --bin efference");
  // An exited process is toned apart, and its end is in the title.
  const exited = renderToStaticMarkup(
    <ProcessChip
      record={record({
        running: false,
        exit: { outcome: { code: 1 }, duration_ms: 12000, at: 1754300012000 },
      })}
      text='"p#4"'
    />,
  );
  expect(exited).toContain("process-chip exited");
  expect(exited).toContain("exit 1");
});

test("a handle with no record degrades to the plain argument", () => {
  // The normal case for a recorded session: the corpus predates the record,
  // and a foreground-only ordinal never had one.
  const markup = renderToStaticMarkup(
    <ProcessChip record={null} text='"p#4"' />,
  );
  expect(markup).toBe("&quot;p#4&quot;");
  expect(markup).not.toContain("chip");
});

test("a missing record is a null, not a throw — and is remembered", async () => {
  const fetchMock = vi.fn().mockResolvedValue(
    json(404, {
      code: "not_found",
      message: "no process p#77 in this session",
    }),
  );
  vi.stubGlobal("fetch", fetchMock);

  expect(await loadProcessRecord(77)).toBeNull();
  // 404 is durable — an id never minted stays never minted — so the second
  // chip on the same handle asks nothing.
  expect(await loadProcessRecord(77)).toBeNull();
  expect(fetchMock).toHaveBeenCalledTimes(1);
  expect((fetchMock.mock.calls[0] as [string])[0]).toBe("/v1/processes/77");
});

test("a record that resolves is fetched once for every chip on the handle", async () => {
  const fetchMock = vi.fn(() =>
    Promise.resolve(json(200, record({ id: 78, running: false }))),
  );
  vi.stubGlobal("fetch", fetchMock);

  const [a, b] = await Promise.all([
    loadProcessRecord(78),
    loadProcessRecord(78),
  ]);
  expect(a?.command).toBe("cargo run --release --bin efference");
  expect(b?.id).toBe(78);
  expect(fetchMock).toHaveBeenCalledTimes(1);
});

test("a transport failure leaves the handle resolvable later", async () => {
  vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new Error("offline")));
  expect(await loadProcessRecord(79)).toBeNull();

  const fetchMock = vi.fn(() => Promise.resolve(json(200, record({ id: 79 }))));
  vi.stubGlobal("fetch", fetchMock);
  expect((await loadProcessRecord(79))?.id).toBe(79);
});

/** The transcript's head for a call, chips and all, at `columns`. */
function head(
  call: { id: string; name: string; input: unknown },
  onOpenFile: (path: string, id: string) => unknown = () => undefined,
  columns: number | null = null,
) {
  return renderToStaticMarkup(
    <CallHead
      layout={layoutSignature(callView(call.name, call.input), columns)}
      chip={(arg, text) => (
        <ArgValue
          call={call}
          arg={arg}
          text={text}
          onOpenFile={(path, id) => void onOpenFile(path, id)}
        />
      )}
    />,
  );
}

test("a path argument carries the file-view link, on the argument itself", () => {
  const call = { id: "toolu_1", name: "write", input: { path: "src/api.rs" } };
  const opened: [string, string][] = [];
  const markup = head(call, (path, id) => opened.push([path, id]));
  // Inside the signature — one inline flow, no trailing slot beside it.
  expect(markup).toContain('class="call-sig"');
  expect(markup).toContain("path-chip");
  expect(markup).toContain("view src/api.rs as it is now on disk");
  expect(markup).toContain("⤢");
  // Positional: write's path reads without its key, and still chips.
  expect(markup).not.toContain("call-arg-key");
  expect(opened).toEqual([]);
});

test("a read's paths still carry no chip, so its signature is untouched", () => {
  const call = {
    id: "toolu_2",
    name: "read",
    input: { paths: ["a.md", "b.rs"] },
  };
  expect(head(call)).not.toContain("chip");
});

test("an argument pulled out of the signature carries no chip", () => {
  // Past three lines of a 40-column head, the path is content, not an
  // argument — it renders below, and the signature has nothing to link.
  const path = `src/${"deep/".repeat(30)}file.rs`;
  const call = { id: "toolu_3", name: "write", input: { path } };
  const markup = head(call, undefined, 40);
  expect(markup).not.toContain("chip");
  expect(markup.replace(/<[^>]+>/g, "")).toContain("write()");
});

test("a chip's command is shortened to a recognizable head", () => {
  expect(chipCommand("cargo test")).toBe("cargo test");
  expect(chipCommand("cargo run --release --bin efference -- --serve")).toBe(
    "cargo run --release --bin effere …",
  );
  expect(chipCommand("cargo watch\n-x test")).toBe("cargo watch");
});
