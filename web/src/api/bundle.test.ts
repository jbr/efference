// @vitest-environment jsdom
import { expect, test } from "vitest";
import {
  BundleWatch,
  CHECK_GAP_MS,
  sameAssets,
  shellAssets,
} from "./bundle.ts";

const shell = (html: string) =>
  shellAssets(new DOMParser().parseFromString(html, "text/html"));

/** One build's shell: the entry script and the stylesheet, plus the icons and
 * manifest a build leaves alone. Vite names the two by their content, so a
 * changed asset is a changed shell. */
const shellHtml = (entry: string, style = "b0") => `
  <link rel="stylesheet" href="/assets/index-${style}.css">
  <script type="module" src="/assets/index-${entry}.js"></script>
  <link rel="icon" href="/icon.svg">
  <link rel="manifest" href="/manifest.webmanifest">`;

test("a shell names the scripts and stylesheets it runs, and nothing else", () => {
  expect(shell(shellHtml("a1"))).toEqual([
    "/assets/index-a1.js",
    "/assets/index-b0.css",
  ]);
});

test("a preload link the page grew at runtime is not part of its shell", () => {
  // Vite's built runtime appends one of these per chunk a dynamic import
  // pulls in, so a used page names more than the shell it loaded with; reading
  // them as shell would report every page stale (docs/web-client.md).
  const withPreload = `
    <link rel="modulepreload" href="/assets/chunk-c9.js">
    ${shellHtml("a1")}`;
  expect(shell(withPreload)).toEqual(shell(shellHtml("a1")));
});

test("the same build is the same set of assets in any order", () => {
  expect(
    sameAssets(
      ["/assets/b.js", "/assets/a.css"],
      ["/assets/a.css", "/assets/b.js"],
    ),
  ).toBe(true);
});

test("a build that changed one asset is a different build", () => {
  expect(sameAssets(shell(shellHtml("a1")), shell(shellHtml("a2")))).toBe(
    false,
  );
  // A stylesheet-only change is still a changed build.
  expect(
    sameAssets(shell(shellHtml("a1", "b1")), shell(shellHtml("a1", "b2"))),
  ).toBe(false);
  // So is a dropped or added file: the count alone decides this one.
  expect(sameAssets(["/assets/a.js"], [])).toBe(false);
});

/** A watch over a mutable answer, with the clock under the test's control. */
function watch(running: string[], served: () => string[] | null) {
  const seen: string[] = [];
  const clock = { now: 0 };
  const ask = { count: 0 };
  const subject = new BundleWatch(
    {
      running: () => running,
      served: async () => {
        ask.count += 1;
        return served();
      },
      now: () => clock.now,
    },
    (state) => seen.push(state),
  );
  return { subject, seen, clock, ask };
}

test("the host serving this page's own shell is no update", async () => {
  const { subject, seen } = watch(shell(shellHtml("a1")), () =>
    shell(shellHtml("a1")),
  );
  await subject.check();
  expect(subject.answer).toBe("current");
  expect(seen).toEqual(["current"]);
});

test("a shell the host serves with other assets is an update", async () => {
  const { subject, seen } = watch(shell(shellHtml("a1")), () =>
    shell(shellHtml("a2")),
  );
  await subject.check();
  expect(subject.answer).toBe("stale");
  expect(seen).toEqual(["stale"]);
});

test("stale is final for the page, whatever a later check says", async () => {
  let served = shell(shellHtml("a2"));
  const { subject, clock } = watch(shell(shellHtml("a1")), () => served);
  await subject.check();
  expect(subject.answer).toBe("stale");
  // The page cannot become the other build by asking again; only a reload can.
  served = shell(shellHtml("a1"));
  clock.now += CHECK_GAP_MS;
  await subject.check();
  expect(subject.answer).toBe("stale");
});

test("a host that could not be asked leaves the answer unknown, and asks again", async () => {
  const { subject, clock, ask } = watch(shell(shellHtml("a1")), () => null);
  await subject.check();
  expect(subject.answer).toBe("unknown");
  // The next connect is past the gap and asks afresh.
  clock.now += CHECK_GAP_MS;
  await subject.check();
  expect(ask.count).toBe(2);
});

test("one answer stands for a host restart's burst of connects", async () => {
  const { subject, clock, ask } = watch(shell(shellHtml("a1")), () =>
    shell(shellHtml("a1")),
  );
  await subject.check();
  await subject.check();
  await subject.check();
  expect(ask.count).toBe(1);
  clock.now += CHECK_GAP_MS;
  await subject.check();
  expect(ask.count).toBe(2);
});

test("a check already running is joined, not repeated", async () => {
  let finish!: () => void;
  const running = shell(shellHtml("a1"));
  const subject = new BundleWatch({
    running: () => running,
    served: () =>
      new Promise<string[]>((resolve) => {
        finish = () => resolve(running);
      }),
    now: () => 0,
  });
  const first = subject.check();
  const second = subject.check();
  finish();
  await Promise.all([first, second]);
  expect(subject.answer).toBe("current");
});
