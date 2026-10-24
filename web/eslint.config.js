import js from "@eslint/js";
import tseslint from "typescript-eslint";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";
import prettier from "eslint-config-prettier";

// `fetch` and `EventSource` are not called directly in `web/src`
// (docs/web-client.md), as globals or as properties of the global objects.
// Every request goes through `apiFetch` (src/api/http.ts), and a stream is a
// fetch read with `sseData` (src/api/stream.ts), so how a request is made is
// decided in one function.
const bareFetch = {
  name: "fetch",
  message:
    "Requests go through apiFetch (src/api/http.ts) — see docs/web-client.md.",
};
const eventSource = {
  name: "EventSource",
  message:
    "Streams are fetch plus sseData (src/api/stream.ts), never EventSource — see docs/web-client.md.",
};
// The same names reached through `window`, `globalThis`, or `self`.
const onGlobalObjects = ({ name, message }) =>
  ["window", "globalThis", "self"].map((object) => ({
    object,
    property: name,
    message,
  }));

// typescript-eslint refuses to load against TypeScript 7, which ships no
// compiler API. The package.json aliases install both side by side:
// `typescript` -> @typescript/typescript6 (the 6.0 API these plugins import)
// and `@typescript/native` -> typescript@7 (the `tsc` used by
// `pnpm typecheck`/`build`). Drop the alias once typescript-eslint supports
// >=7.1 — https://github.com/typescript-eslint/typescript-eslint/issues/10940
export default tseslint.config(
  { ignores: ["dist"] },
  js.configs.recommended,
  tseslint.configs.recommended,
  // react-hooks 7's flat config declares `plugins` as an array, which ESLint 10
  // rejects; register the plugin as an object ourselves and borrow its rules.
  {
    plugins: { "react-hooks": reactHooks },
    rules: reactHooks.configs["recommended-latest"].rules,
  },
  reactRefresh.configs.vite,
  // The service worker runs in a worker global scope, not the page's
  // (public/sw.js; docs/web-client.md "Installability").
  {
    files: ["public/sw.js"],
    languageOptions: {
      globals: {
        self: "readonly",
        caches: "readonly",
        fetch: "readonly",
        URL: "readonly",
        Promise: "readonly",
      },
    },
  },
  // `fetch` is called in one place: the entry point itself. `public/sw.js` is
  // outside `src` and calls the worker's own `fetch` (block above).
  {
    files: ["src/**/*.{ts,tsx}"],
    rules: {
      "no-restricted-globals": ["error", bareFetch, eventSource],
      "no-restricted-properties": [
        "error",
        ...onGlobalObjects(bareFetch),
        ...onGlobalObjects(eventSource),
      ],
    },
  },
  {
    files: ["src/api/http.ts"],
    rules: {
      "no-restricted-globals": ["error", eventSource],
      "no-restricted-properties": ["error", ...onGlobalObjects(eventSource)],
    },
  },
  {
    rules: {
      "@typescript-eslint/explicit-function-return-type": "off",
      // Nothing here may turn untrusted text into markup: every body on the
      // page is model- or tool-authored, and the page holds a session
      // credential for a control channel that runs commands
      // (docs/web-client.md). JSX text nodes and React-escaped
      // attributes are the only sanctioned sinks; these two are the doors out
      // of that, so reaching for one has to be an argument rather than a
      // habit.
      "no-restricted-syntax": [
        "error",
        {
          selector: "JSXAttribute[name.name='dangerouslySetInnerHTML']",
          message:
            "Untrusted content must never become markup — see docs/web-client.md.",
        },
        {
          selector:
            "AssignmentExpression > MemberExpression[property.name=/^(inner|outer)HTML$/]",
          message:
            "Untrusted content must never become markup — see docs/web-client.md.",
        },
      ],
    },
  },
  prettier,
);
