import js from "@eslint/js";
import tseslint from "typescript-eslint";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";
import prettier from "eslint-config-prettier";

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
  {
    rules: {
      "@typescript-eslint/explicit-function-return-type": "off",
      // Nothing here may turn untrusted text into markup: every body on the
      // page is model- or tool-authored, and the page holds a session
      // credential for a control channel that runs commands
      // (backlog/web/untrusted-output.md). JSX text nodes and React-escaped
      // attributes are the only sanctioned sinks; these two are the doors out
      // of that, so reaching for one has to be an argument rather than a
      // habit.
      "no-restricted-syntax": [
        "error",
        {
          selector: "JSXAttribute[name.name='dangerouslySetInnerHTML']",
          message:
            "Untrusted content must never become markup — see backlog/web/untrusted-output.md.",
        },
        {
          selector:
            "AssignmentExpression > MemberExpression[property.name=/^(inner|outer)HTML$/]",
          message:
            "Untrusted content must never become markup — see backlog/web/untrusted-output.md.",
        },
      ],
    },
  },
  prettier,
);
