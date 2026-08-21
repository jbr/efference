// Syntax highlighting (docs/web-client.md). Shiki, in its fine-grained
// bundle: the app embeds into the binary at compile time, so the full
// grammar set is not affordable and the languages are exactly the ones
// `render/parse.ts`'s `langForPath` can name.
//
// Two choices worth keeping:
//
// - **The JavaScript raw engine over precompiled grammars, not the Oniguruma
//   WASM one.** No `.wasm` to ship or fetch, and no `wasm-unsafe-eval` needed
//   in the CSP the bind serves (docs/web-client.md). *Raw*, not *regex*: the
//   regex engine carries a transpiler that converts each grammar's Oniguruma
//   patterns to JavaScript `RegExp` at load time — 147 kB of main chunk.
//   `@shikijs/langs-precompiled` ships grammars whose patterns are already
//   JavaScript regex literals, so the raw engine runs them with no
//   transpiler. The literals carry the `v` (unicodeSets) flag: an ES2024
//   floor — Chrome 112, Safari 17, Firefox 116 — which the desktop browsers
//   this targets clear.
// - **Tokens, not HTML.** `codeToTokens` returns plain
//   `{ content, color }[]` grouped by line, which the callers render as
//   spans. `codeToHtml` would hand back a markup string, and nothing on
//   this page turns untrusted text into markup. The line grouping is also
//   exactly the shape the numbered-gutter and diff views already use.
//
// Highlighting is best-effort: until the grammars finish loading, and
// forever if they fail to, callers render the same plain text they did
// before. Nothing here is on the path to *reading* a transcript.

import {
  createHighlighterCore,
  type GrammarState,
  type HighlighterCore,
} from "shiki/core";
import { createJavaScriptRawEngine } from "shiki/engine/javascript";
import { useEffect, useMemo, useSyncExternalStore } from "react";
import { CSS_VARIABLES_THEME, cssVariablesTheme } from "./shikiTheme.ts";

/** One entry per language `langForPath` returns. Dynamic imports so the
 * grammars are their own chunks rather than main-bundle weight; precompiled,
 * so the engine needs no transpiler (see above). Every language named here is
 * published precompiled — a missing one would have to fall back to
 * `@shikijs/langs` *and* drag the regex engine back in for all of them. */
const LANGS = [
  import("@shikijs/langs-precompiled/rust"),
  import("@shikijs/langs-precompiled/toml"),
  import("@shikijs/langs-precompiled/markdown"),
  import("@shikijs/langs-precompiled/json"),
  import("@shikijs/langs-precompiled/javascript"),
  import("@shikijs/langs-precompiled/typescript"),
  import("@shikijs/langs-precompiled/python"),
  import("@shikijs/langs-precompiled/bash"),
  import("@shikijs/langs-precompiled/yaml"),
  import("@shikijs/langs-precompiled/html"),
  import("@shikijs/langs-precompiled/css"),
];

export interface Token {
  content: string;
  color?: string;
}

let highlighter: HighlighterCore | null = null;
let started = false;
const listeners = new Set<() => void>();

function announce() {
  for (const listener of listeners) listener();
}

function start() {
  if (started) return;
  started = true;
  void createHighlighterCore({
    langs: LANGS,
    themes: [cssVariablesTheme],
    engine: createJavaScriptRawEngine(),
  })
    .then((core) => {
      highlighter = core;
      announce();
    })
    .catch(() => {
      // Stays null; every caller falls back to plain text.
    });
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** A `(code, lang) => lines of tokens` function, or `null` while the
 * grammars are still loading (or if they failed). Re-renders its caller once
 * when highlighting becomes available. The one theme is the css-variables
 * theme (ui/shikiTheme.ts): token "colors" are var() references the browser
 * resolves against the active theme's tokens, so a theme swap recolors every
 * block with no re-render and no re-tokenize.
 *
 * The returned function is **stable** across renders (it changes only when the
 * highlighter does) so that callers can memoize their tokens on it. That is
 * load-bearing, not tidiness: a store update re-renders the whole transcript,
 * so an unstable identity re-tokenizes every code block in the session on
 * every streamed token. */
export function useHighlight():
  ((code: string, lang: string) => Token[][] | null) | null {
  useEffect(start, []);
  const core = useSyncExternalStore(
    subscribe,
    () => highlighter,
    () => null,
  );
  return useMemo(() => {
    if (core === null) return null;
    return (code: string, lang: string) => {
      if (!core.getLoadedLanguages().includes(lang)) return null;
      try {
        return core.codeToTokens(code, { lang, theme: CSS_VARIABLES_THEME })
          .tokens;
      } catch {
        return null;
      }
    };
  }, [core]);
}

/**
 * Tokenizing for an append-only buffer — a tool call's contents while they
 * stream. Re-tokenizing the whole buffer per delta is O(n) work per chunk and
 * O(n²) over a large file write; Shiki's grammar state makes tokenizing
 * resumable at a line boundary instead. Tokens for complete lines are kept,
 * and each update tokenizes only what arrived since the last complete line.
 *
 * The correctness property (pinned by test): the result is identical to
 * tokenizing the whole buffer at once.
 */
export class StreamingTokenizer {
  /** Token lines for [`prefix`], which is complete-line (`\n`-terminated)
   * and only ever extends. */
  private done: Token[][] = [];
  private prefix = "";
  /** The grammar's state at the end of [`prefix`] — what makes the next
   * lines tokenizable without the ones already done. */
  private state: GrammarState | undefined = undefined;

  constructor(
    private readonly core: HighlighterCore,
    private readonly lang: string,
    private readonly theme: string,
  ) {}

  tokens(text: string): Token[][] {
    if (!text.startsWith(this.prefix)) {
      // Not an append: a new call reusing this component slot, or a buffer
      // that hit the store's cap and slid. Start over.
      this.done = [];
      this.prefix = "";
      this.state = undefined;
    }
    const rest = text.slice(this.prefix.length);
    const cut = rest.lastIndexOf("\n");
    if (cut >= 0) {
      const closed = this.core.codeToTokensBase(rest.slice(0, cut), {
        lang: this.lang,
        theme: this.theme,
        grammarState: this.state,
      });
      this.done.push(...closed);
      this.state = this.core.getLastGrammarState(closed);
      this.prefix = text.slice(0, this.prefix.length + cut + 1);
    }
    // The line still open re-tokenizes every time — it is one line.
    const open = this.core.codeToTokensBase(text.slice(this.prefix.length), {
      lang: this.lang,
      theme: this.theme,
      grammarState: this.state,
    });
    return [...this.done, ...open];
  }
}

/** [`StreamingTokenizer`] as a hook: `null` outside highlighting's reach
 * (grammars loading or failed, unknown language), tokens otherwise. The
 * tokenizer is memoized on language and theme, so it persists across the
 * deltas of one value and starts over when either changes. Advancing it
 * inside the memo is safe: the same text tokenizes to the same result, so a
 * double invocation redoes only the open line. If React ever drops the memo,
 * the fresh tokenizer's first call is one whole-buffer pass — the cost this
 * hook exists to avoid paying *per delta*, acceptable once. */
export function useStreamingTokens(
  text: string,
  lang: string | null,
): Token[][] | null {
  useEffect(start, []);
  const core = useSyncExternalStore(
    subscribe,
    () => highlighter,
    () => null,
  );
  const tokenizer = useMemo(
    () =>
      core !== null && lang !== null && core.getLoadedLanguages().includes(lang)
        ? new StreamingTokenizer(core, lang, CSS_VARIABLES_THEME)
        : null,
    [core, lang],
  );
  return useMemo(() => {
    if (tokenizer === null) return null;
    try {
      return tokenizer.tokens(text);
    } catch {
      return null;
    }
  }, [tokenizer, text]);
}
