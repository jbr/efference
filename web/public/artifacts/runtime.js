/* global window, document, console, matchMedia, MutationObserver, ResizeObserver, setTimeout, Element */

(() => {
  const themeMessage = "efference:artifact:theme";
  const resizeMessage = "efference:artifact:resize";
  const themes = new Set(["after-dark", "lilac"]);
  const root = document.documentElement;

  // The stylesheet's prose rules apply under `.prose`, as they do in the
  // transcript; here the whole document is the prose (docs/artifacts.md).
  root.classList.add("prose");

  // What the host hands an artifact (docs/artifacts.md), each by a script it
  // adds to the page: `await efference.load("d3")` resolves to a built-in
  // library's global, and `await efference.file("data/sizes.csv")` to a file's
  // text from the session's working directory, read when the page loads. The
  // host answers each with a script calling back into this object. A request it
  // refuses also puts a line at the top of the page: the agent that wrote the
  // call cannot see it fail, and the operator can tell it.
  const requests = new Map();
  const waiting = new Map();
  const notice = (message) => {
    const line = document.createElement("p");
    line.className = "efference-notice";
    line.textContent = message;
    (document.body ?? root).prepend(line);
  };
  const settle = (key, value) => {
    waiting.get(key)?.resolve(value);
    waiting.delete(key);
  };
  const fail = (key, message) => {
    waiting.get(key)?.reject(new Error(message));
    waiting.delete(key);
    notice(message);
    console.error(message);
  };
  const request = (key, src, call) => {
    if (!requests.has(key)) {
      requests.set(
        key,
        new Promise((resolve, reject) => {
          waiting.set(key, { resolve, reject });
          const script = document.createElement("script");
          script.src = src;
          script.onerror = () => fail(key, `${call} could not reach the host.`);
          document.head.append(script);
        }),
      );
    }
    return requests.get(key);
  };
  window.efference = {
    load(name) {
      const key = String(name);
      return request(
        `load:${key}`,
        `/artifacts/_efference/lib/${encodeURIComponent(key)}`,
        `efference.load("${key}")`,
      );
    },
    file(path) {
      const key = String(path);
      return request(
        `file:${key}`,
        `${window.location.pathname}/file?path=${encodeURIComponent(key)}`,
        `efference.file("${key}")`,
      );
    },
    __loaded(name, library) {
      settle(`load:${name}`, library);
    },
    __missing(name, available) {
      fail(
        `load:${name}`,
        `efference.load("${name}"): no such library here. Available: ${available.join(", ")}.`,
      );
    },
    __file(path, text) {
      settle(`file:${path}`, text);
    },
    __fileRefused(path, reason) {
      fail(`file:${path}`, `efference.file("${path}"): ${reason}.`);
    },
  };

  // Opened in a tab of its own there is no parent to name a theme, so follow
  // the system's, as the app does before it knows the operator's choice.
  if (window.parent === window) {
    const light = matchMedia("(prefers-color-scheme: light)");
    const follow = () => {
      root.dataset.theme = light.matches ? "lilac" : "after-dark";
      root.style.colorScheme = light.matches ? "light" : "dark";
    };
    follow();
    light.addEventListener("change", follow);
  }

  window.addEventListener("message", (event) => {
    if (event.source !== window.parent) return;
    const message = event.data;
    if (
      message === null ||
      typeof message !== "object" ||
      message.type !== themeMessage ||
      !themes.has(message.theme) ||
      message.colorScheme !== (message.theme === "lilac" ? "light" : "dark")
    ) {
      return;
    }
    root.dataset.theme = message.theme;
    root.style.colorScheme = message.colorScheme;
  });

  // The parent's window never sees a wheel or touch over this frame — only the
  // scroll the browser chains to it once this page can scroll no further, which
  // alone reads there as layout, and the transcript would pin itself back to
  // its tail (docs/artifacts.md). So report that moment: upward input that
  // nothing here handled or scrolled. Checked after dispatch, so an artifact's
  // own handlers have had their turn.
  const scrolledUpMessage = "efference:artifact:scrolled-up";
  const scrolledHere = (target) => {
    for (
      let element = target instanceof Element ? target : null;
      element !== null && element !== root;
      element = element.parentElement
    ) {
      if (element.scrollTop > 0) return true;
    }
    return (document.scrollingElement?.scrollTop ?? 0) > 0;
  };
  const reportUpward = (event) =>
    setTimeout(() => {
      if (!event.defaultPrevented && !scrolledHere(event.target)) {
        window.parent.postMessage({ type: scrolledUpMessage }, "*");
      }
    });
  window.addEventListener(
    "wheel",
    (event) => {
      if (event.deltaY < 0 && !event.ctrlKey) reportUpward(event);
    },
    { passive: true },
  );
  let touchY = null;
  window.addEventListener(
    "touchstart",
    (event) => {
      touchY = event.touches[0]?.clientY ?? null;
    },
    { passive: true },
  );
  window.addEventListener(
    "touchmove",
    (event) => {
      const next = event.touches[0]?.clientY ?? null;
      if (next !== null && touchY !== null && next > touchY) {
        reportUpward(event);
      }
      touchY = next;
    },
    { passive: true },
  );

  let queued = false;
  let sent = 0;
  const sendSize = () => {
    queued = false;
    // The root element's box: in standards mode it is as tall as the content,
    // collapsed child margins included, and is not stretched to the frame, so a
    // page that gets shorter shrinks its frame. The body's box would miss those
    // margins, and quirks mode stretches both to the viewport — the server adds a
    // doctype to every artifact so a fragment never renders in quirks mode.
    const height = Math.ceil(
      document.documentElement.getBoundingClientRect().height,
    );
    // Only a change: an animation reports on every mutation.
    if (Number.isFinite(height) && height > 0 && height !== sent) {
      sent = height;
      window.parent.postMessage({ type: resizeMessage, height }, "*");
    }
  };
  // A timer rather than an animation frame, and mutations as well as resizes:
  // the browser runs neither animation frames nor resize observers in a frame
  // scrolled out of view, which would leave it at its opening height until it
  // came into view and then resize under the operator's eyes. Measured while
  // off screen, it takes its height where scroll anchoring absorbs it.
  const scheduleSize = () => {
    if (queued) return;
    queued = true;
    setTimeout(sendSize);
  };

  const start = () => {
    const observer = new ResizeObserver(scheduleSize);
    observer.observe(document.documentElement);
    if (document.body !== null) observer.observe(document.body);
    new MutationObserver(scheduleSize).observe(document.documentElement, {
      childList: true,
      subtree: true,
      attributes: true,
      characterData: true,
    });
    window.addEventListener("load", scheduleSize);
    window.addEventListener("resize", scheduleSize);
    scheduleSize();
    if (document.fonts?.ready !== undefined) {
      void document.fonts.ready.then(scheduleSize);
    }
  };

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", start, { once: true });
  } else {
    start();
  }
})();
