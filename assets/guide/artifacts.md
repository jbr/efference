# Artifacts: charts, diagrams and small pages in the transcript

A fenced block whose info string is exactly `html-artifact` renders in the web transcript
as a live page in a sandboxed frame. Use one when a picture says it better than prose or
a table: a chart of numbers you computed, a diagram of how parts connect, a small view the
operator can interact with.

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

- **Web client:** the page, with a Source toggle beside it. An `html` fence, and
  `html-artifact` followed by anything else, stays source.
- **TUI:** your source as a code block, and a link that opens the page in a local browser.
  Keep the source readable.
- **Your context:** the source stays in the conversation for the rest of the session.
  Keep an artifact small: draw with a few lines of script over an inline data array rather
  than writing every element out.

## The sandbox

An artifact runs on an opaque origin with scripts allowed and little else:

- Inline `<style>` and `<script>` work. `eval`, `new Function` and string timers do not.
- No network. `fetch`, and scripts, styles, images or fonts from anywhere else, are
  refused, so a library from a CDN will not load: write the few lines you need. An image
  works as a `data:` or `blob:` URL.
- No storage: `localStorage`, `sessionStorage`, IndexedDB and cookies throw.
- No form submission and no popups.

## Size

The frame is as wide as the transcript column, which runs from a phone's width to a
desktop's: give an SVG a `viewBox` so it scales down, and a `max-width` at its drawn size
so its text never scales up past reading size. It is as tall as your page, up to 70%
of the window, past which the page scrolls inside the frame. Do not size `html` or `body`
to `100%` or `100vh`: the frame takes its height from the page, so a page that takes its
height from the frame cannot shrink to its content.

## The stylesheet every artifact gets

Before your content, the page receives the transcript's own stylesheet:

- Plain HTML — headings, paragraphs, lists, tables, `code`, `blockquote` — takes the
  transcript's prose styling. Write semantic elements and they fit in.
- The page is transparent and edge to edge; the transcript shows through.
- `button`, `input` and `select` use the interface font.
- An `<svg>` that names no `fill` paints in the text colour; a shape that names its own
  fill keeps it.
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
- **Data:** `--series-1` to `--series-6`, a categorical palette at one lightness, so no
  series looks louder than another; `--series-1` is the accent.
- **Type:** `--font-prose` (Newsreader) for reading text · `--font-ui` (IBM Plex Sans) for
  labels and controls · `--font-mono` (IBM Plex Mono) for values, numbers and identifiers.
