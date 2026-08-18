// The streaming tokenizer's one property: feeding a buffer in arbitrary
// chunks yields exactly the tokens of one whole-buffer pass. Everything else
// about it — grammar state at line boundaries, the open line redone per call —
// is implementation; this equality is the contract.

import { createHighlighterCore } from "shiki/core";
import { createJavaScriptRegexEngine } from "shiki/engine/javascript";
import { beforeAll, describe, expect, it } from "vitest";
import { StreamingTokenizer } from "./highlight.ts";

const THEME = "github-dark";

// Multi-line constructs on purpose: the raw string and the doc comment span
// lines, so a tokenizer that lost grammar state at a boundary would color the
// continuation wrong and fail the equality.
const CODE = `//! A doc comment spanning
//! two lines.
fn main() {
    let s = r#"a raw
string over lines"#;
    println!("{s}");
}
`;

let core: Awaited<ReturnType<typeof createHighlighterCore>>;

/** What the renderer consumes — content and color. `offset` is relative to
 * the string each pass tokenized, so it legitimately differs between an
 * incremental pass and a whole-buffer one, and nothing reads it. */
const drawn = (lines: { content: string; color?: string }[][]) =>
  lines.map((line) => line.map(({ content, color }) => ({ content, color })));

beforeAll(async () => {
  core = await createHighlighterCore({
    langs: [import("@shikijs/langs/rust")],
    themes: [import("@shikijs/themes/github-dark")],
    engine: createJavaScriptRegexEngine(),
  });
});

const whole = () => core.codeToTokensBase(CODE, { lang: "rust", theme: THEME });

describe("StreamingTokenizer", () => {
  it("matches the one-shot tokens at every prefix, fed byte by byte", () => {
    const tokenizer = new StreamingTokenizer(core, "rust", THEME);
    for (let end = 1; end <= CODE.length; end++) {
      const prefix = CODE.slice(0, end);
      expect(drawn(tokenizer.tokens(prefix))).toEqual(
        drawn(core.codeToTokensBase(prefix, { lang: "rust", theme: THEME })),
      );
    }
  });

  it("matches the one-shot tokens fed in uneven chunks", () => {
    const tokenizer = new StreamingTokenizer(core, "rust", THEME);
    let fed = "";
    let last: unknown = null;
    for (const size of [3, 17, 1, 40, 7, CODE.length]) {
      fed = CODE.slice(0, Math.min(CODE.length, fed.length + size));
      last = drawn(tokenizer.tokens(fed));
    }
    expect(last).toEqual(drawn(whole()));
  });

  it("is idempotent for a repeated buffer", () => {
    // A StrictMode double render calls with the same text twice.
    const tokenizer = new StreamingTokenizer(core, "rust", THEME);
    tokenizer.tokens(CODE);
    expect(drawn(tokenizer.tokens(CODE))).toEqual(drawn(whole()));
  });

  it("starts over when the buffer is not an append", () => {
    const tokenizer = new StreamingTokenizer(core, "rust", THEME);
    tokenizer.tokens(CODE);
    const other = 'fn other() { println!("changed"); }\n';
    expect(drawn(tokenizer.tokens(other))).toEqual(
      drawn(core.codeToTokensBase(other, { lang: "rust", theme: THEME })),
    );
  });
});
