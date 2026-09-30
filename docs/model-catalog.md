# The model catalog

The host keeps each connection's model listing, reduced to one row shape, in the user
state database and serves it to clients. A client shows the rows as suggestions: a model
absent from the catalog is still accepted as a typed id, and a listing in no case
authorizes or refuses a model. Modules: `src/catalog.rs` (+ `src/catalog/`) for storage,
age, and refresh; `src/daemon/host/catalog.rs` for the routes; the per-provider
normalization in `src/api/provider/` (`row.rs`, `openrouter.rs`, `anthropic.rs`,
`codex.rs`, `listing.rs`), selected in `models.rs`.

## The table

`model_catalog` in `~/.efference/state.db` (the file the history store uses,
[transport.md](transport.md)) has one row per connection: connection name (primary key),
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
folded), `name`, `created` (unix seconds), `context` (tokens), `price` (`input`/`output`,
USD per million tokens), `reasoning`, `images` (image input). An undeclared fact is
absent. `recent` is added at read time, not stored.

- **Names** lose OpenRouter's author prefix (`Z.ai: GLM 5.3 Flash` becomes `GLM 5.3
  Flash`: everything through the first `": "`). An empty name, or one equal to the id, is
  omitted.
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
| OpenRouter | `GET /api/v1/models` (sent without `anthropic-version`) | name, created, context_length, pricing, `architecture.input_modalities`, `supported_parameters`/`reasoning` |
| Anthropic | `GET /v1/models?limit=100` | display_name, created_at (RFC 3339), max_input_tokens, `capabilities.image_input.supported` |
| OpenAI Codex | the Codex catalog (also the capability source, [transport.md](transport.md)) | display_name, context window, input modalities, reasoning levels |
| OpenAI platform | `GET /v1/models` | created |
| Described endpoint | `GET {base}/models`, read field by field | created, name/display_name, context, image modality |

## Recent ranks

`GET /inference/catalog` and `/inference/catalog/:connection` take the project scope the
history store keys on (`cwd`, optional `config`) and fold the ranked picks of
`recent_all` (up to 20, counted across connections, project picks first) onto the
matching (connection, model) row as `recent`. A pick the listing does not name — a typed
id — becomes a row with only `connection`, `model`, the OpenRouter `group` derived from
the id, and `recent`. `GET /inference/recent-picks` remains for the connection picker's
cross-connection list.

## Routes

[control-api.md](control-api.md) has the shapes. Every configured connection appears; one
with no usable credential (or disabled) reports status `unavailable` with the reason and
no read is attempted. The list route never waits on a provider; the single-connection
route awaits a refresh when its copy is older than `max_age` (default the TTL, floor 60
seconds).
