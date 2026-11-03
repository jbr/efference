import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { bearer } from "./api/bearer.ts";
import { startAttention } from "./attention.ts";
import { startBundleWatch } from "./api/bundle.ts";
import { registerServiceWorker } from "./pwa.ts";
import { resubscribeOnOpen } from "./push.ts";
import { App } from "./ui/App.tsx";
import { installLinkNavigation } from "./ui/route.ts";
import { initTheme } from "./ui/theme.ts";
import { installMatteFinish } from "./theme/matte.ts";
import "./index.css";

initTheme();
const stopMatteFinish = installMatteFinish();
if (import.meta.hot) import.meta.hot.dispose(stopMatteFinish);
installLinkNavigation();
registerServiceWorker();
if (
  import.meta.env.DEV &&
  new URLSearchParams(location.search).has("overflow")
) {
  void import("./overflowReport.ts").then((m) => m.installOverflowReport());
}
// Every request the page makes carries the stored bearer (api/bearer.ts), so
// nothing that makes one starts until storage has been read.
void bearer.load().then(start);

function start() {
  startAttention();
  startBundleWatch();
  void resubscribeOnOpen(
    "serviceWorker" in navigator ? navigator.serviceWorker : undefined,
  );
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
}
