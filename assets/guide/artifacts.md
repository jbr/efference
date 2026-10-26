# Artifacts: charts, diagrams and small pages in the transcript

A fenced block whose info string is exactly `html-artifact` renders in the web transcript
as a live page in a sandboxed frame. Use one when a picture says it better than prose or
a table: a chart of numbers you computed, a diagram of how parts connect, a small view the
operator can interact with.

**The info string is the whole switch.** Anything after `html-artifact` on the fence line —
`html-artifact chart`, `html-artifact preview` — makes it an ordinary code block.

````html-artifact
<h3>Build time by crate</h3>
<svg id="chart" viewBox="0 0 480 112" style="max-width: 480px" role="img"
     aria-label="Build time by crate"></svg>
<script>
  const rows = [["core", 41], ["daemon", 63], ["tui", 28], ["web", 17]];
  const longest = Math.max(...rows.map(([, seconds]) => seconds));
  const chart = document.getElementById("chart");
  rows.forEach(([name, seconds], i) => {
    const y = i * 28;
    const width = (seconds / longest) * 320;
    const fill = seconds === longest ? "var(--accent)" : "var(--fg-faint)";
    chart.insertAdjacentHTML("beforeend", `
      <text x="0" y="${y + 15}" style="font: 12px var(--font-ui); fill: var(--fg-dim)">${name}</text>
      <rect x="64" y="${y}" width="${width}" height="20" style="fill: ${fill}"/>
      <text x="${70 + width}" y="${y + 15}" style="font: 12px var(--font-mono)">${seconds}s</text>`);
  });
</script>
````

Write a fragment, as above, or a whole document; the server adds what a page needs.

## What the operator sees

- **Web client:** the page, with a Source toggle beside it.
- **TUI:** your source as a code block, and a link that opens the page in a local browser.
  Keep the source readable: semantic elements, a title on what you draw, a comment where
  the script is not obvious.

## What it costs you

The source stays in the conversation and is sent again with every request for the rest of
the session, so a richer artifact is context you pay for on every later turn. Aggregate
before you draw — 30 bins rather than 300 points, unless the detail is the point — and
draw with a few lines of script over an inline data array rather than writing every
element out.

## Checking your work

You cannot see the page, and nothing comes back to you: no preview, no console, no word
that a script threw or a resource was refused. A script that fails leaves part of the page
empty. Write what you can predict — plain SVG and DOM calls over data you computed, labels
that fit by construction — and when it matters that the operator sees what you meant, ask
them what they see.

## The sandbox

The page runs on an opaque origin with scripts allowed.

You can use:

- inline `<style>` and `<script>`, including `<script type="module">`;
- the DOM: `createElement`, `innerHTML`, `insertAdjacentHTML`, template literals, event
  listeners, `requestAnimationFrame`, timers;
- SVG, `<canvas>` and WebAssembly; `eval` and `new Function` work too;
- the libraries below;
- images as `data:` or `blob:` URLs.

You cannot use:

- the network: `fetch`, `import()` of a URL, and scripts, styles, images or fonts from
  anywhere else. A library from a CDN does not load, and nothing tells you so; use one
  built in, or write the few lines you need;
- storage: `localStorage`, `sessionStorage`, IndexedDB and cookies throw;
- form submission or popups.

## Libraries

Three libraries are built in, one version each. `await efference.load(name)` resolves to
the library's usual global; write against the version named here, not a newer one:

- `efference.load("d3")` — d3 7.9.0, resolving to `d3`.
- `efference.load("vega-embed")` — vega-embed 6.29.0, with Vega 5.33.1 and Vega-Lite
  5.23.0, resolving to `vegaEmbed`. Vega-Lite specs use the v5 schema.
- `efference.load("chart.js")` — Chart.js 4.4.9, resolving to `Chart`.

```html
<div id="chart"></div>
<script type="module">
  const vegaEmbed = await efference.load("vega-embed");
  await vegaEmbed("#chart", {
    $schema: "https://vega.github.io/schema/vega-lite/v5.json",
    data: { values: [{ crate: "core", seconds: 41 }, { crate: "tui", seconds: 28 }] },
    mark: "bar",
    encoding: { x: { field: "seconds", type: "quantitative" }, y: { field: "crate", type: "nominal" } },
  }, { actions: false });
</script>
```

A name that is not one of these rejects, and the page shows a line naming the ones there
are, so the operator can tell you.

## Size

- **Width** is the transcript column's, which runs from a phone's width to a desktop's.
  Give an SVG a `viewBox` so it scales down, and a `max-width` at its drawn size so its
  text never scales up past reading size. Anything outside the `viewBox` is clipped, so
  leave room inside it for labels at the edges: above the tallest bar, past the longest.
- **Height** follows your page. The frame opens at a default height, then takes the height
  of the page's content as the page reports it, measured on the root element, so it grows
  and shrinks with whatever is in normal flow. Past 70% of the window the page scrolls
  inside the frame. Leave heights to the content: a fixed-height wrapper fixes the frame's
  height with it, and `100%` or `100vh` on `html` or `body` takes the height from the
  frame, so the page can never shrink to its content.

## The stylesheet every artifact gets

Before your content, the page receives the transcript's own stylesheet:

- Plain HTML — headings, paragraphs, lists, tables, `code`, `blockquote` — takes the
  transcript's prose styling. Write semantic elements and they fit in.
- The page is transparent and edge to edge; the transcript shows through.
- `button`, `input` and `select` use the interface font.
- An `<svg>` that names no `fill` paints in the text colour, and every shape or group inside
  it without a fill of its own inherits that; one that names a fill keeps it.
- The operator's theme reaches the page live: After Dark is dark, Lilac is light, and the
  operator can switch while your page is open. Take every colour from the variables below,
  and never hardcode black or white text.

All of it sits in a cascade layer, so any rule you write wins, without `!important`.
Departing from the transcript's look is your choice to make — when the content calls for
it, or the operator asks for something else.

### Variables

- **Text:** `--fg` body text · `--fg-bright` emphasis, the value that matters ·
  `--fg-muted` · `--fg-dim` secondary labels, axis text · `--fg-faint` the least important.
- **Surfaces and lines:** `--bg-page` the transcript's background · `--bg-raised` a raised
  surface · `--bg-inset` a well, as code sits in · `--border-subtle` · `--border` gridlines
  and dividers · `--border-strong` axes and table rules.
- **Meaning:** `--accent` the one thing that needs attention · `--warn` a problem ·
  `--diff-add` positive, added · `--diff-del` negative, removed.
- **Data:** `--series-1` to `--series-6`, six hues spaced around the colour wheel at one
  lightness, so no series looks louder than another. In order: the accent (pink in After
  Dark, heather in Lilac), violet, blue, teal, green, amber.
- **Type:** `--font-prose` (Newsreader) for reading text · `--font-ui` (IBM Plex Sans) for
  labels and controls · `--font-mono` (IBM Plex Mono) for values, numbers and identifiers.
