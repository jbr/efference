import { GrainShader } from "./grainShader.ts";

// Keep the surface list paired with matte.css (docs/web-client.md).
const selector =
  "body, .g-head, .top-bar, .dock, .composer, .status-bar, .composer-input, .card, .gate-float";

/** Bound backing-store memory while preserving the chosen CSS-space grain size. */
export function textureSize(width: number, height: number, pixelRatio: number) {
  const ratio = Math.min(
    pixelRatio || 1,
    2,
    4096 / Math.max(width, height),
    Math.sqrt(4_194_304 / (width * height)),
  );
  return {
    width: Math.max(1, Math.round(width * ratio)),
    height: Math.max(1, Math.round(height * ratio)),
    ratio,
  };
}

type Texture = { users: Set<Surface>; url?: string };
type Surface = { element: HTMLElement; visible: boolean; texture?: Texture };

/** One owner per page. Only newly inserted surface subtrees are discovered;
 * streaming text mutations neither scan the document nor trigger GPU draws.
 * CSS gradients and flat colors remain if WebGL or image encoding fails. */
export function installMatteFinish() {
  const surfaces = new Map<HTMLElement, Surface>();
  const textures = new Map<string, Texture>();
  const pending = new Set<Surface>();
  const forced = matchMedia("(forced-colors: active)");
  const print = matchMedia("print");
  const output = document.createElement("canvas");
  const probe = document.createElement("canvas");
  probe.width = probe.height = 1;
  const colorContext = probe.getContext("2d", { willReadFrequently: true });
  let renderer: GrainShader | undefined;
  let frame = 0;
  let stopped = false;
  let failed = !colorContext;

  const enabled = () =>
    !stopped && !failed && !forced.matches && !print.matches;
  function release(surface: Surface) {
    const texture = surface.texture;
    surface.texture = undefined;
    surface.element.style.removeProperty("--matte-texture");
    surface.element.style.removeProperty("--matte-page-size");
    if (!texture) return;
    texture.users.delete(surface);
    if (texture.users.size === 0) {
      if (texture.url) URL.revokeObjectURL(texture.url);
      for (const [key, value] of textures) {
        if (value === texture) textures.delete(key);
      }
    }
  }
  function disable() {
    failed = true;
    pending.clear();
    for (const surface of surfaces.values()) release(surface);
    renderer?.destroy();
    renderer = undefined;
    output.width = output.height = 0;
  }
  function draw(surface: Surface) {
    if (!colorContext || !surface.element.isConnected) return;
    const element = surface.element;
    const page = element === document.body;
    const rect = element.getBoundingClientRect();
    const width = page ? window.innerWidth : rect.width;
    const height = page ? window.innerHeight : rect.height;
    if (width <= 0 || height <= 0) {
      release(surface);
      return;
    }
    colorContext.clearRect(0, 0, 1, 1);
    colorContext.fillStyle = getComputedStyle(element).backgroundColor;
    colorContext.fillRect(0, 0, 1, 1);
    const [r, g, b, a] = colorContext.getImageData(0, 0, 1, 1).data;
    // Transparent surfaces must keep showing what is behind them.
    if (a !== 255) {
      release(surface);
      return;
    }
    const size = textureSize(width, height, window.devicePixelRatio);
    const key = [r, g, b, size.width, size.height, size.ratio].join(",");
    let texture = textures.get(key);
    if (texture === surface.texture && texture) return;
    release(surface);
    if (page)
      element.style.setProperty(
        "--matte-page-size",
        `${size.width / size.ratio}px ${size.height / size.ratio}px`,
      );
    if (texture) {
      texture.users.add(surface);
      surface.texture = texture;
      if (texture.url)
        element.style.setProperty("--matte-texture", `url("${texture.url}")`);
      return;
    }
    texture = { users: new Set([surface]) };
    textures.set(key, texture);
    surface.texture = texture;
    renderer ??= new GrainShader();
    renderer.draw(output, {
      base: [r, g, b],
      lightness: 0.02,
      color: 0.03,
      preserve: true,
      scale: 0.15,
      size,
    });
    const rendered = texture;
    output.toBlob((blob) => {
      if (!enabled() || rendered.users.size === 0) return;
      if (!blob) {
        disable();
        return;
      }
      const url = URL.createObjectURL(blob);
      rendered.url = url;
      for (const user of rendered.users)
        user.element.style.setProperty("--matte-texture", `url("${url}")`);
    });
  }
  function flush() {
    frame = 0;
    const batch = [...pending];
    pending.clear();
    if (!enabled()) return;
    try {
      for (const surface of batch) {
        if (surface.visible) draw(surface);
      }
    } catch {
      disable();
    }
  }
  function schedule(surface: Surface) {
    if (!enabled() || !surface.visible) return;
    pending.add(surface);
    frame ||= requestAnimationFrame(flush);
  }
  const resize = new ResizeObserver((entries) => {
    for (const entry of entries) {
      const surface = surfaces.get(entry.target as HTMLElement);
      if (surface) schedule(surface);
    }
  });
  const visibility = new IntersectionObserver((entries) => {
    for (const entry of entries) {
      const surface = surfaces.get(entry.target as HTMLElement);
      if (!surface) continue;
      surface.visible = entry.isIntersecting;
      if (surface.visible) schedule(surface);
      else release(surface);
    }
  });
  function discover(node: Node) {
    if (!(node instanceof HTMLElement)) return;
    const candidates = [...node.querySelectorAll<HTMLElement>(selector)];
    if (node.matches(selector)) candidates.push(node);
    for (const element of candidates) {
      if (surfaces.has(element)) continue;
      const surface = { element, visible: false };
      surfaces.set(element, surface);
      resize.observe(element);
      visibility.observe(element);
    }
  }
  function resized() {
    for (const surface of surfaces.values()) schedule(surface);
  }
  function invalidate() {
    for (const surface of surfaces.values()) {
      release(surface);
      schedule(surface);
    }
    if (!enabled()) {
      renderer?.destroy();
      renderer = undefined;
      output.width = output.height = 0;
    }
  }
  const mutations = new MutationObserver((records) => {
    let removed = false;
    for (const record of records) {
      if (record.type === "attributes") {
        if (record.target === document.documentElement) invalidate();
        else {
          const surface = surfaces.get(record.target as HTMLElement);
          if (surface) schedule(surface);
        }
      } else {
        for (const node of record.addedNodes) discover(node);
        removed ||= record.removedNodes.length > 0;
      }
    }
    if (removed) {
      for (const [element, surface] of surfaces) {
        if (element.isConnected) continue;
        release(surface);
        pending.delete(surface);
        resize.unobserve(element);
        visibility.unobserve(element);
        surfaces.delete(element);
      }
    }
  });
  mutations.observe(document.documentElement, {
    childList: true,
    subtree: true,
    attributes: true,
    attributeFilter: ["data-theme", "class"],
  });
  discover(document.body);
  window.addEventListener("resize", resized);
  forced.addEventListener("change", invalidate);
  print.addEventListener("change", invalidate);

  return () => {
    stopped = true;
    cancelAnimationFrame(frame);
    mutations.disconnect();
    resize.disconnect();
    visibility.disconnect();
    window.removeEventListener("resize", resized);
    forced.removeEventListener("change", invalidate);
    print.removeEventListener("change", invalidate);
    disable();
    probe.width = probe.height = 0;
  };
}
