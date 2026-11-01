// The positional honesty rule, case-for-case with the TUI's
// `src/clients/tui/projection/partial/test.rs`: a settled value is final, an arriving
// value renders only as content, and no prefix ever claims what the whole
// document contradicts.

import { describe, expect, it } from "vitest";
import { hasStructure, partialInput, streamingLang } from "./partial.ts";

describe("partialInput", () => {
  // The whole point: the identifying argument is settled — and therefore
  // renderable as identity — long before the big one finishes arriving.
  it("settles a closed pair while the next is still arriving", () => {
    const input = partialInput('{"path":"src/api.rs","contents":"use st')!;
    expect(input.settled).toEqual({ path: "src/api.rs" });
    expect(input.arriving).toEqual({ key: "contents", text: "use st" });
  });

  // The claim the honesty rule turns on: a half-arrived path must never reach
  // the settled set, because `write src/api.r` is well-formed, plausible, and
  // false. It is the last key, so it is in flight, so it renders as content
  // or not at all.
  it("never settles a half-arrived value", () => {
    const input = partialInput('{"path":"src/api.r')!;
    expect(input.settled).toEqual({});
    expect(input.arriving).toEqual({ key: "path", text: "src/api.r" });
  });

  it("settles a closed value before the object closes", () => {
    const input = partialInput('{"path":"src/api.rs"')!;
    expect(input.settled).toEqual({ path: "src/api.rs" });
    expect(input.arriving).toBeNull();
  });

  it("has nothing arriving for a complete document", () => {
    const input = partialInput('{"path":"a.rs","contents":"fn main() {}"}')!;
    expect(Object.keys(input.settled)).toEqual(["path", "contents"]);
    expect(input.arriving).toBeNull();
  });

  it("keeps arrival order", () => {
    const input = partialInput(
      '{"intent":"write it","path":"a.rs","contents":"x"}',
    )!;
    expect(Object.keys(input.settled)).toEqual(["intent", "path", "contents"]);
  });

  // Before any structure has arrived there is nothing to claim, and the
  // caller falls back to rendering the raw characters.
  it("yields nothing for a buffer with no structure yet", () => {
    for (const buffer of ["", "{", '{"pa']) {
      const input = partialInput(buffer);
      expect(input === null || !hasStructure(input)).toBe(true);
    }
  });

  // Feeding the buffer one byte at a time, no prefix ever reports a settled
  // value that the complete document disagrees with. This is the invariant
  // that matters across the whole stream, not just at the two or three
  // offsets a hand-written case happens to pick.
  it("never settles a value the whole document contradicts", () => {
    const whole =
      '{"intent":"write the renderer","path":"src/tui/render.rs",' +
      '"append":false,"contents":"fn main() {\\n    println!(\\"hi\\");\\n}\\n"}';
    const complete = partialInput(whole)!;
    expect(complete.arriving).toBeNull();
    const finalKeys = Object.keys(complete.settled);

    for (let end = 1; end <= whole.length; end++) {
      const prefix = partialInput(whole.slice(0, end));
      if (prefix === null) continue;
      for (const [key, value] of Object.entries(prefix.settled)) {
        expect(
          complete.settled[key],
          `at byte ${end}, ${key} settled as a value the document contradicts`,
        ).toEqual(value);
      }
      // And the arriving key, when there is one, is always the *next* key
      // the document has after the settled ones — never an earlier one
      // revisited.
      if (prefix.arriving !== null) {
        expect(
          finalKeys[Object.keys(prefix.settled).length],
          `at byte ${end}, ${prefix.arriving.key} is arriving out of order`,
        ).toBe(prefix.arriving.key);
      }
    }
  });

  // Escapes decode rather than showing through: a rendered `\n` must be a
  // newline, since turning the escaped contents back into lines is what makes
  // the streamed text renderable as text at all.
  it("decodes escapes in the arriving value", () => {
    const input = partialInput(
      '{"path":"a.rs","contents":"fn a() {\\n    b();\\n',
    )!;
    expect(input.arriving?.text).toBe("fn a() {\n    b();\n");
  });

  // The settled set drops the arriving pair — the per-tool renderer must only
  // ever see closed values, since what it produces is an identity claim.
  it("excludes the arriving pair from the settled set", () => {
    const input = partialInput('{"path":"a.rs","contents":"partial')!;
    expect(input.settled).toEqual({ path: "a.rs" });
    expect("contents" in input.settled).toBe(false);
  });
});

describe("streamingLang", () => {
  it("reads the language off a settled path", () => {
    expect(streamingLang({ path: "src/api.rs" })).toBe("rust");
    expect(streamingLang({ file_path: "web/a.ts" })).toBe("typescript");
  });

  it("names nothing without a settled path", () => {
    expect(streamingLang({})).toBeNull();
    expect(streamingLang({ command: "ls" })).toBeNull();
  });
});
