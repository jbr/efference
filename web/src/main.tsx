import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { registerServiceWorker } from "./pwa.ts";
import { App } from "./ui/App.tsx";
import { installLinkNavigation } from "./ui/route.ts";
import { initTheme } from "./ui/theme.ts";
import "./index.css";

initTheme();
installLinkNavigation();
registerServiceWorker();
if (
  import.meta.env.DEV &&
  new URLSearchParams(location.search).has("overflow")
) {
  void import("./overflowReport.ts").then((m) => m.installOverflowReport());
}
createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
