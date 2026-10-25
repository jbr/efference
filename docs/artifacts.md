# Artifacts

An artifact is agent-authored HTML displayed in a sandboxed browser document. An assistant reply opts in with a fenced block whose info string is exactly `html-artifact`; ordinary `html` and `svg` fences, and `html-artifact` followed by anything else, remain source. Artifacts come from committed assistant text only — not user messages, thinking, tool results, or streaming output. Their source stays in the conversation and its projected context.

## Coordinates

An artifact is addressed by its entry's seq, its text block's ordinal among the entry's text blocks, and its ordinal among that block's `html-artifact` fences (`src/artifact.rs`). Fences are found by CommonMark's rules alone — inside any container, and running to the end of their container when nothing closes them — so the web client's renderer (remark), numbering the same block, arrives at the same ordinals. `web/src/ui/artifact-fences.json` holds the cases both parsers are tested against.

## Links and serving

Nothing about an artifact is stored apart from the log entry that holds it. The entry view of a committed assistant entry (`daemon/entry_view.rs`, [control-api.md](control-api.md)) carries one link per fence, derived on every read:

- `url` — `/artifacts/<session>/<seq>/<text_block>/<fence>/<tag>`, root-relative, so a web client loads it from the origin it already uses, local or remote.
- `external_url` — the same path on the host's artifact-only loopback listener, for a local browser; the TUI shows it.

The tag is an HMAC-SHA256 over the session id and the coordinates, under a key the `Session` holds, random per session. Serving a document checks the tag, finds the session among the host's (or the one a session bind serves), reads the entry by seq, and extracts the fence again, so a caller cannot supply HTML. A tag is valid for its own coordinates only. A session that leaves the host takes its key with it, and its links stop resolving with nothing to clean up; a resumed session signs new ones. The links are bearer URLs: anyone holding one can read that artifact while its session is hosted.

The routes sit outside the device-token gate ([web-bind.md](web-bind.md)), admitted by exact shape (`artifacts::is_artifact_route`): a document path, or one of the shared assets under `/artifacts/_efference/`. They are mounted at the web origin's root and on the loopback listener, which the host starts at its first adopt, under its own `Swansong`, and which serves no other route.

## Browser boundary

The embedding iframe uses `sandbox="allow-scripts"`, without `allow-same-origin`. Responses also carry a CSP sandbox, so opening an artifact URL in a tab of its own keeps an opaque origin. Artifact script cannot read the web client's IndexedDB bearer or browser storage. The artifact policy is separate from the web shell's: it permits inline HTML, CSS and JS, and refuses fetches and subresource loads from other origins (`default-src 'none'`, `connect-src 'none'`). It governs the loads a document makes, not navigation of the frame itself, which the browser-boundary spike did not test (journal/2026-10-24-artifact-browser-boundary-spike.md). The artifact response permits same-origin framing; every other response keeps the shell's anti-framing headers.

The parent/runtime message channel carries sizing, theme, and one report of input: the operator scrolling up past the frame's top. A wheel or touch over the frame goes to the frame's document, so the parent's window sees only the scroll the browser chains to it, which tail-following alone would read as layout and pin back ([web-client.md](web-client.md), *Transcript ergonomics*); the runtime reports upward input that nothing in the artifact handled or scrolled, and the parent departs the tail as it would for a wheel over the window. The parent identifies a message by its source window — an opaque frame's origin is `"null"` — and no message can ask the parent to make a request or disclose its bearer. A frame's URL comes from the entry view the client already holds; the web client makes no request on an artifact's behalf.

## Runtime and presentation

The server opens every document with two harness-owned elements, right after its doctype — the stylesheet `/artifacts/_efference/theme.css`, then the runtime `/artifacts/_efference/runtime.js` — and adds `<!doctype html>` when the source has none: a fragment without one would render in quirks mode, where the root element is stretched to the frame and the frame could never shrink to its content.

The stylesheet is built into the binary from the web client's own files under `web/src/theme/`: the self-hosted fonts, then, inside `@layer efference`, the theme tokens, the element defaults, the prose rules, and the artifact baseline — transparent, edge to edge, controls in the interface font, and an `<svg>` that names no fill painting in the text colour. The app imports the same files, so an artifact's styling follows the transcript's when either changes, and, being layered, every rule gives way to any rule an artifact writes. The fonts load from `/fonts/` on the document's own origin. An opaque origin makes those loads CORS requests, so font responses allow any origin and the artifact policy's `font-src` admits `'self'`; the loopback listener serves the same files from the web build, except in `dev-proxy` builds, where its documents fall back to the font stacks' system faces.

The runtime puts `prose` on the root element, so the prose rules apply to the whole document. It applies the parent's theme messages — theme identity and color scheme — or, with no parent, follows the system color scheme. It reports the root element's height — the content's, collapsed margins included — and the parent bounds inline height so oversized content scrolls within the frame rather than growing the transcript without limit.

An entry with artifacts renders text block by text block, as the server numbers them. The transcript keys its Markdown components on the entry's link list and the block's ordinal rather than on the object carrying them, so re-rendering the transcript, as it does on every streamed token, leaves committed frames mounted. The web transcript shows a source view beside each frame, and a fence with no link stays code. The TUI keeps the code block and adds a line naming the external URL.

The guide `efference://guide/artifacts` (`assets/guide/artifacts.md`) tells an agent what an artifact gets — the sandbox, sizing, the stylesheet and its variables — and `src/guide/test.rs` checks that every variable it names is one `tokens.css` defines. The frozen session preamble names the fence and points at the guide ([context.md](context.md)).

File-backed presentation, live reload, agent-side evaluation/screenshots, external-origin permission prompts and a headless browser are separate planned work; none is required to render a fenced artifact.
