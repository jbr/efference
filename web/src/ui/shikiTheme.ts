// The highlighter's one theme: every color is a var(--shiki-*) reference, so
// syntax colors come from the active theme's tokens (theme.css) and a theme
// swap recolors code with no re-tokenize (docs/web-client.md). Shiki copies
// these strings into token colors untouched — the callers put them on React's
// style prop, where the browser resolves the variable. Shiki v4 dropped its
// createCssVariablesTheme factory; this is the same shape, hand-authored,
// with the scope map trimmed to the languages langForPath names. The
// passthrough is pinned by highlight.test.ts.

import type { ThemeRegistrationAny } from "shiki/core";

const v = (name: string) => `var(--shiki-${name})`;

export const CSS_VARIABLES_THEME = "css-variables";

export const cssVariablesTheme: ThemeRegistrationAny = {
  name: CSS_VARIABLES_THEME,
  settings: [
    {
      settings: { foreground: v("foreground") },
    },
    {
      scope: ["comment", "string.comment"],
      settings: { foreground: v("token-comment") },
    },
    {
      scope: ["keyword", "storage.type", "storage.modifier"],
      settings: { foreground: v("token-keyword") },
    },
    {
      scope: ["string", "punctuation.definition.string"],
      settings: { foreground: v("token-string") },
    },
    {
      scope: [
        "constant",
        "constant.numeric",
        "variable.other.enummember",
        "support.constant",
      ],
      settings: { foreground: v("token-constant") },
    },
    {
      scope: [
        "entity.name.type",
        "entity.name.namespace",
        "entity.name.tag",
        "support.type",
        "support.class",
      ],
      settings: { foreground: v("token-type") },
    },
    {
      scope: ["entity.name.function", "support.function"],
      settings: { foreground: v("token-function") },
    },
    {
      scope: ["variable.parameter"],
      settings: { foreground: v("token-parameter") },
    },
    {
      scope: ["punctuation", "meta.brace"],
      settings: { foreground: v("token-punctuation") },
    },
    {
      scope: ["markup.underline.link", "string.other.link"],
      settings: { foreground: v("token-link") },
    },
  ],
};
