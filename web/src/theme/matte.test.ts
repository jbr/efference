// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { installMatteFinish, textureSize } from "./matte.ts";

const { draw, destroy } = vi.hoisted(() => ({
  draw: vi.fn(),
  destroy: vi.fn(),
}));
vi.mock("./grainShader.ts", () => ({
  GrainShader: class {
    draw = draw;
    destroy = destroy;
  },
}));

let stop: (() => void) | undefined;
let intersect: (
  entries: { target: Element; isIntersecting: boolean }[],
) => void;
let resize: (entries: { target: Element }[]) => void;
let frames: FrameRequestCallback[];
let blobs: BlobCallback[];
let forced: MediaQueryList & { matches: boolean };
const createUrl = vi.fn();
const revokeUrl = vi.fn();

beforeEach(() => {
  vi.clearAllMocks();
  draw.mockReset();
  frames = [];
  blobs = [];
  const header = document.createElement("div");
  header.className = "g-head";
  document.body.replaceChildren(header);
  vi.spyOn(header, "getBoundingClientRect").mockReturnValue({
    width: innerWidth,
    height: innerHeight,
  } as DOMRect);
  document.body.removeAttribute("style");
  document.documentElement.removeAttribute("data-theme");
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
    frames.push(callback);
    return frames.length;
  });
  vi.stubGlobal("cancelAnimationFrame", vi.fn());
  vi.stubGlobal(
    "ResizeObserver",
    class {
      constructor(callback: typeof resize) {
        resize = callback;
      }
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );
  vi.stubGlobal(
    "IntersectionObserver",
    class {
      constructor(callback: typeof intersect) {
        intersect = callback;
      }
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );
  vi.stubGlobal("matchMedia", (query: string) => {
    const media = Object.assign(new EventTarget(), { matches: false });
    if (query.includes("forced-colors")) forced = media as typeof forced;
    return media;
  });
  const context = {
    fillStyle: "",
    clearRect() {},
    fillRect() {},
    getImageData: () => ({ data: new Uint8ClampedArray([22, 19, 29, 255]) }),
  };
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(
    context as never,
  );
  vi.spyOn(HTMLCanvasElement.prototype, "toBlob").mockImplementation(
    (callback) => {
      blobs.push(callback);
    },
  );
  createUrl.mockImplementation(
    () => `blob:grain-${createUrl.mock.calls.length}`,
  );
  vi.stubGlobal(
    "URL",
    Object.assign(URL, {
      createObjectURL: createUrl,
      revokeObjectURL: revokeUrl,
    }),
  );
});

afterEach(() => {
  stop?.();
  stop = undefined;
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function header() {
  return document.querySelector<HTMLElement>(".g-head")!;
}
function enter(...elements: Element[]) {
  intersect(elements.map((target) => ({ target, isIntersecting: true })));
}
function flush() {
  const batch = frames;
  frames = [];
  for (const callback of batch) callback(0);
}
function encode() {
  const batch = blobs;
  blobs = [];
  for (const callback of batch) callback(new Blob(["texture"]));
}
const texture = (element: HTMLElement) =>
  element.style.getPropertyValue("--matte-texture");

it("bounds the backing store without changing CSS-space grain size", () => {
  expect(textureSize(1200, 900, 1)).toEqual({
    width: 1200,
    height: 900,
    ratio: 1,
  });
  expect(textureSize(393, 852, 3)).toEqual({
    width: 786,
    height: 1704,
    ratio: 2,
  });
  const large = textureSize(8000, 4000, 2);
  expect(large.width).toBeLessThanOrEqual(4096);
  expect(large.width * large.height).toBeLessThanOrEqual(4_200_000);
  expect(Math.abs(large.width - 8000 * large.ratio)).toBeLessThanOrEqual(0.5);
});

it("shares a viewport-sized page texture, draws only on demand and uses the selected finish", () => {
  stop = installMatteFinish();
  expect(draw).not.toHaveBeenCalled();
  enter(document.body, header());
  flush();
  expect(draw).toHaveBeenCalledTimes(1);
  expect(draw.mock.calls[0][1]).toEqual({
    base: [22, 19, 29],
    lightness: 0.02,
    color: 0.03,
    preserve: true,
    scale: 0.15,
    size: textureSize(innerWidth, innerHeight, devicePixelRatio),
  });
  encode();
  expect(texture(document.body)).toBe('url("blob:grain-1")');
  expect(texture(header())).toBe(texture(document.body));
  expect(document.body.style.getPropertyValue("--matte-page-size")).toBe(
    `${innerWidth}px ${innerHeight}px`,
  );
  window.dispatchEvent(new Event("scroll"));
  expect(frames).toHaveLength(0);
  resize([{ target: document.body }]);
  flush();
  expect(draw).toHaveBeenCalledTimes(1);
  expect(frames).toHaveLength(0);
});

it("releases only the last shared user and destroys the single renderer on cleanup", () => {
  stop = installMatteFinish();
  enter(document.body, header());
  flush();
  encode();
  intersect([{ target: header(), isIntersecting: false }]);
  expect(revokeUrl).not.toHaveBeenCalled();
  expect(texture(header())).toBe("");
  stop();
  stop = undefined;
  expect(revokeUrl).toHaveBeenCalledWith("blob:grain-1");
  expect(texture(document.body)).toBe("");
  expect(destroy).toHaveBeenCalledTimes(1);
});

it("discards an encoding result that completes after disposal", () => {
  stop = installMatteFinish();
  enter(document.body);
  flush();
  stop();
  stop = undefined;
  encode();
  expect(createUrl).not.toHaveBeenCalled();
  expect(texture(document.body)).toBe("");
});

it("invalidates theme textures and ignores stale encodings", async () => {
  stop = installMatteFinish();
  enter(document.body);
  flush();
  const stale = blobs.shift()!;
  document.documentElement.dataset.theme = "lilac";
  await Promise.resolve();
  flush();
  stale(new Blob(["old"]));
  expect(createUrl).not.toHaveBeenCalled();
  encode();
  expect(draw).toHaveBeenCalledTimes(2);
  expect(texture(document.body)).toBe('url("blob:grain-1")');
});

it("discovers new surfaces but streaming text does not redraw existing ones", async () => {
  stop = installMatteFinish();
  enter(document.body);
  flush();
  encode();
  const card = document.createElement("div");
  card.className = "card";
  vi.spyOn(card, "getBoundingClientRect").mockReturnValue({
    width: 300,
    height: 80,
  } as DOMRect);
  document.body.append(card);
  await Promise.resolve();
  enter(card);
  flush();
  encode();
  expect(draw).toHaveBeenCalledTimes(2);
  card.append("streaming text");
  await Promise.resolve();
  flush();
  expect(draw).toHaveBeenCalledTimes(2);
  card.remove();
  await Promise.resolve();
  expect(revokeUrl).toHaveBeenCalledWith("blob:grain-2");
});

it("leaves flat backgrounds when WebGL fails, without retrying each resize", () => {
  draw.mockImplementation(() => {
    throw new Error("No WebGL");
  });
  stop = installMatteFinish();
  enter(document.body);
  flush();
  expect(texture(document.body)).toBe("");
  resize([{ target: document.body }]);
  flush();
  expect(draw).toHaveBeenCalledTimes(1);
  expect(destroy).toHaveBeenCalledTimes(1);
});

it("removes textures in forced colors and regenerates when it ends", () => {
  stop = installMatteFinish();
  enter(document.body);
  flush();
  encode();
  forced.matches = true;
  forced.dispatchEvent(new Event("change"));
  flush();
  expect(texture(document.body)).toBe("");
  expect(revokeUrl).toHaveBeenCalledWith("blob:grain-1");
  forced.matches = false;
  forced.dispatchEvent(new Event("change"));
  flush();
  encode();
  expect(texture(document.body)).toBe('url("blob:grain-2")');
});
