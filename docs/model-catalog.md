# The model catalog

The host keeps each connection's model listing, reduced to one row shape, in the user
state database and serves it to clients. A client shows the rows as suggestions: a model
absent from the catalog is still accepted as a typed id, and a listing in no case
authorizes or refuses a model. Modules: `src/catalog.rs` (+ `src/catalog/`) for storage,
age, and refresh; `src/daemon/host/catalog.rs` for the routes; the per-provider
normalization in `src/api/provider/` (`row.rs`, `openrouter.rs`, `anthropic.rs`,
`codex.rs`, `listing.rs`, `typesafe.rs`), selected in `models.rs`.

## The table

`model_catalog` in the user state database ([state-store.md](state-store.md), beside the
history table the picks live in — [transport.md](transport.md)) has one row per
connection: connection name (primary key),
provider name, `fetched_at` (unix seconds), and the normalized rows as JSON. It is keyed by
connection name rather than base URL because Anthropic and OpenAI listings are scoped to
the account behind the credential. A stored row whose provider differs from the
configured connection's provider is ignored and deleted. Like the history store, every
operation is best-effort; with no database path the catalog is held in memory only.

## Age and refresh

`CATALOG_TTL` is 24 hours, a constant. A read serves the stored copy with its age. A
connection with no copy, or a copy past the TTL, is refreshed in the background; one
refresh per connection is in flight at a time, and a caller that needs the answer awaits
the same read instead of starting another. A read that fails keeps the old copy and
records its error, which the status reports until a read succeeds; a background refresh
is not retried within a minute of a failure. One read is bounded at 30 seconds.

## The row

`connection`, `model` (the provider-native id a draft carries), and whichever of these
the source declares: `group` (OpenRouter's author, the id prefix before `/`, a leading `~`
folded), `name`, `description` (provider prose), `created` (unix seconds), `context`
(tokens), `price` (`input`/`output`, USD per million tokens), `reasoning`, `images`
(image input), `line` (the model line the provider files it under) and `lifecycle`
(`active`, `deprecated`, `retired`). An undeclared fact is
absent. `recent` is added at read time, not stored.

- **Names** lose OpenRouter's author prefix (`Z.ai: GLM 5.3 Flash` becomes `GLM 5.3
  Flash`: everything through the first `": "`). An empty name, or one equal to the id, is
  omitted.
- **Descriptions** retain the source text verbatim; missing, null, empty, or
  whitespace-only descriptions are omitted. OpenRouter, Codex, TypeSafe, and described
  endpoints supply them where declared. The Anthropic and OpenAI platform listings
  currently supply none; the platform inventory does not borrow Codex descriptions.
- **Order:** dated rows newest first, then undated rows, each in the source's order among
  equals. Codex dates nothing, so its order is the backend's.
- **Filtering:** an OpenRouter entry whose `supported_parameters` does not name `tools` is
  dropped, as is an id ending `:batch`; `:free` stays. The Codex catalog drops the
  entries it hides from menus (`visibility: hide`, `supported_in_api: false`). The
  `openai` platform listing is unfiltered. Dropped models stay reachable as a typed id.
- **Price:** OpenRouter's per-token decimal strings times 10^6; a negative value (`-1`,
  the source's "unknown") leaves the price absent.

| Provider | Source | Facts read |
|---|---|---|
| OpenRouter | `GET /api/v1/models?output_modalities=text,decisions` (sent without `anthropic-version`) | name, description, created, context_length (`0` reads as absent), pricing, `architecture.input_modalities`/`output_modalities`, `supported_parameters`/`reasoning` |
| Anthropic | `GET /v1/models?limit=100` | display_name, created_at (RFC 3339), max_input_tokens, `capabilities.image_input.supported`, `line`, `lifecycle` |
| OpenAI Codex | the Codex catalog (also the capability source, [transport.md](transport.md)) | display_name, description, context window, input modalities, reasoning levels |
| TypeSafe | `GET /v1/models` | name (model id), description, release_date (RFC 3339); every row `decisions` |
| OpenAI platform | `GET /v1/models` | created |
| Described endpoint | `GET {base}/models`, read field by field | created, name/display_name, description, context, image modality |
| Described `systemone` endpoint | none | none — an empty catalog |

## The spotter default

Where a session's spotter falls through to the main connection's cheap band
([permissions.md](permissions.md)), resolution reads the connection's held listing
first (`Provider::newest_spotter_model`, through `InferenceRegistry::with_catalog`):
on Anthropic, the newest row by `created` whose `line` is `haiku` and whose
`lifecycle` is `active`. Anthropic's docs say not to infer the line from the id, so
nothing here parses one. Resolution reads only what is held: a listing not yet
fetched, a copy stored before rows carried `line`, or a provider whose listing names
no lines falls back to the compiled-in `Provider::default_spotter_model`. The
offline verbs' registry (`src/bin/efference/inference.rs`) opens the same stored
catalog, so `dev spotter` and `dev spotter-eval` default to what a host session
would get. The next read of the
listing (the picker's, or any past [`CATALOG_TTL`](#age-and-refresh)) brings the
answer up to date.

## Kind and role

A row has a `kind`: `text` (absent on the wire) or `decisions`, a model that answers typed
questions about a state and produces no turn — only the spotter can use one
([permissions.md](permissions.md)). A row inherits it from where it was listed: every row of
a provider that serves no turns is `decisions`, and an OpenRouter entry declares it in
`architecture.output_modalities`. OpenRouter's catalog read asks for
`output_modalities=text,decisions`, because its default listing holds text models only, and
an entry with the `decisions` modality skips the `tools` filter (it declares no supported
parameters at all).

`ModelRole` (`src/catalog/view.rs`) is the one statement of which role may be offered what:
`main` and `triage` take text rows on connections whose provider serves turns, `spotter`
takes every row. The catalog routes apply it as `?role=` ([control-api.md](control-api.md));
the terminal form reads the unfiltered catalog once and applies the same function per
picker, and the web picker passes its role. A connection declared `dialect = "systemone"`
publishes no listing, so for the spotter it appears with no rows and the typed id is the whole
menu.

## Recent ranks

`GET /inference/catalog` and `/inference/catalog/:connection` take the project scope the
history store keys on (`cwd`, optional `config`) and fold the ranked picks of
`recent_all` (up to 20, counted across connections, project picks first) onto the
matching (connection, model) row as `recent`. A pick the listing does not name — a typed
id — becomes a row with only `connection`, `model`, the OpenRouter `group` derived from
the id, `recent`, and a `kind` taken from the connection. The catalog routes are the only source of recents: the terminal
picker ([tui.md](tui.md)) ranks its root and connection views from them.

## Routes

[control-api.md](control-api.md) has the shapes. Every configured connection appears; one
with no usable credential (or disabled) reports status `unavailable` with the reason and
no read is attempted. The list route never waits on a provider; the single-connection
route awaits a refresh when its copy is older than `max_age` (default the TTL, floor 60
seconds).
