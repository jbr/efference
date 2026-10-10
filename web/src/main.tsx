import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { startAttention } from "./attention.ts";
import { registerServiceWorker } from "./pwa.ts";
import { resubscribeOnOpen } from "./push.ts";
import { App } from "./ui/App.tsx";
import { installLinkNavigation } from "./ui/route.ts";
import { initTheme } from "./ui/theme.ts";
import "./index.css";

initTheme();
installLinkNavigation();
registerServiceWorker();
startAttention();
void resubscribeOnOpen(
  "serviceWorker" in navigator ? navigator.serviceWorker : undefined,
);
if (
  import.meta.env.DEV &&
  new URLSearchParams(location.search).has("overflow")
) {
  void import("./overflowReport.ts").then((m) => m.installOverflowReport());
}
const root = createRoot(document.getElementById("root")!);
// The design gallery (src/gallery/Gallery.tsx) replaces the app, and is
// dead code outside dev builds.
if (
  import.meta.env.DEV &&
  new URLSearchParams(location.search).has("gallery")
) {
  void import("./gallery/Gallery.tsx").then(({ Gallery }) =>
    root.render(
      <StrictMode>
        <Gallery />
      </StrictMode>,
    ),
  );
} else {
  root.render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}
