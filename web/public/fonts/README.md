# Self-hosted fonts

Declared by src/theme.css; served same-origin because the CSP is
`default-src 'self'`. Official builds, unmodified — subsetting counts as
modification under the OFL's reserved-name clause, so the whole files ship
with their license texts beside them (backlog/web/theme.md has the
reasoning):

- `newsreader-var.woff2` / `newsreader-italic-var.woff2` — Newsreader
  variable (opsz, wght), Production Type — `OFL-Newsreader.txt`
- `plex-mono-regular.woff2` / `plex-mono-italic.woff2` /
  `plex-mono-medium.woff2` — IBM Plex Mono 400 / 400 italic / 500, IBM —
  `OFL-IBMPlexMono.txt`

IBM Plex Sans is deliberately absent: `--font-ui` falls back to `system-ui`,
and nothing visible leans on the sans. Adding a face means dropping the file
here and declaring it in src/theme.css.
