import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Single-origin is the target: the daemon's TCP bind serves this app and /v1
// together (trillium-frontend; `cargo run --features dev-proxy` for HMR). This
// proxy only matters when running Vite standalone against a separate
// `efference serve --bind` on :7878; harmless under dev-proxy.
export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      "/v1": "http://localhost:7878",
      "/auth": "http://localhost:7878",
      "/pair": "http://localhost:7878",
    },
  },
});
