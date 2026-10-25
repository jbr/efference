# Artifacts

An artifact is agent-authored HTML displayed in a sandboxed browser document. An assistant reply opts in with a closed `html-artifact` fenced block. Ordinary `html` and `svg` fences remain source code. Artifacts are sourced from committed assistant text, not user messages, thinking, tool results, or incomplete streaming output. Their source remains part of the conversation and its projected context.

## Addressing and serving

The session control API resolves `GET /v1/artifacts/:seq/:text_block/:fence` to a URL descriptor. `text_block` is the zero-based ordinal among the assistant entry's text blocks; `fence` is the zero-based ordinal among all fenced code blocks in that text block. The server checks the stored entry and extracts the selected closed `html-artifact` fence; callers cannot supply replacement HTML through this API. The host exposes the same control route under `/sessions/<id>`.

The descriptor's `url` is a same-origin path under `/artifacts/<token>/`, usable by a remote web client without a second public listener. `external_url` names the artifact-only loopback listener for a local browser. Resolving a fence repeatedly produces the same token during the process lifetime; restarting the host requires resolving it again. Tokens authorize access to one artifact, not the session's control API. They are bearer URLs: anyone who obtains one can read that artifact while its token is valid.

Artifact routes sit outside the device-token gate ([web-bind.md](web-bind.md)). They serve the selected HTML and harness-owned runtime/theme assets, not arbitrary project files. A token does not grant access to neighboring artifacts or authenticated routes. Unknown tokens, unrelated paths and traversal do not resolve to files.

## Browser boundary

The embedding iframe uses `sandbox="allow-scripts"`, without `allow-same-origin`. Responses also carry a CSP sandbox, so opening an artifact URL in a separate tab retains an opaque origin. Artifact script cannot read the web client's IndexedDB bearer or browser storage. The artifact policy is separate from the authenticated web shell's policy; it permits inline HTML/CSS/JS but blocks external network resources by default. The artifact response permits framing; other responses retain the shell's anti-framing headers.

The parent/runtime message channel carries presentation data only: sizing and theme. The parent validates a message's source against the iframe window; an opaque frame's `"null"` origin cannot identify it. No message can ask the parent to make an authenticated request or disclose its bearer. Authenticated artifact resolution happens in the client, using the stored assistant entry's coordinates, before loading the frame.

## Runtime and presentation

The server injects `_efference/runtime.js`; an artifact can link `_efference/theme.css`. These assets live in `web/public/artifacts/` and are embedded by the server. The runtime reports document height, and the parent bounds inline height so oversized content scrolls within the frame rather than growing the transcript without limit. Theme messages carry the client's theme identity and color scheme. Theme CSS exposes the client's design variables for self-contained responsive content.

The web transcript preserves a source view alongside the rendered document. The TUI keeps the code block and resolves an external URL for opening in a local browser. The frozen session preamble names the opt-in fence and its sandbox constraints ([context.md](context.md)).

File-backed presentation, live reload, agent-side evaluation/screenshots, external-origin permission prompts and a headless browser are separate planned work; none is required to render a fenced artifact.
