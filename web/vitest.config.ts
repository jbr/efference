import { defineConfig } from "vitest/config";

// Separate from vite.config.ts so the build config and test config stay
// independent (trillium-frontend's `vite build` reads vite.config.ts). The
// suite covers the api/session layers — pure TS, no DOM — so the environment
// stays node until a component test needs jsdom.
export default defineConfig({
  test: {
    environment: "node",
  },
});
