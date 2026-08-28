import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { registerServiceWorker } from "./pwa.ts";
import { App } from "./ui/App.tsx";
import { initTheme } from "./ui/theme.ts";
import "./index.css";

initTheme();
registerServiceWorker();
createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
