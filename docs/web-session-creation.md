# Session creation in the web client

The overview's **New session** button and each workspace's **+** open full-page
launch surfaces in place of the session list. `web/src/ui/SessionList.tsx` owns
the launch draft; `NewSessionForm.tsx` renders location, settings, and model
selection. The routes, requests, and host behavior are part of the same control
API used by other clients ([control-api.md](control-api.md)).

## Navigation and draft ownership

`ui/newSessionNavigation.ts` identifies the surface from query parameters on `/`:

- `?new=location` selects a working directory.
- `?new=settings` edits session settings.
- `?new=model&role=main|spotter|triage` selects a model for that role.

The query changes the client surface without adding a server SPA route. Forward
navigation pushes a history entry; `popstate` updates the rendered page. An
entry records its predecessor and, for flows entered from the overview, its
owned history depth. **Back to settings** uses the predecessor when it matches,
otherwise replaces the current surface. **Cancel** unwinds the owned launch
entries to the overview. A direct entry without owned depth cancels by removing
the launch query parameters from the current URL.

The shared `ui/AppHeader.tsx` wordmark links home on every launch surface. An
ordinary click exits through the same history/draft-preserving path as Cancel;
modified clicks retain native link behavior.

The draft contains raw overrides, not the host's resolved values. It survives
Cancel and reopening while the overview is mounted; reloading discards it. A
model or settings URL without a draft renders location selection first.
Changing location preserves the settings overrides. The launch component owns
separate browsing memories for each role: namespace path, query, keyboard index,
and scroll offset. Those memories last through visits to settings within the
launch flow, but not through cancellation and remounting.

## Location

General entry opens **Where will you work?** without automatically choosing the
most recent directory. A workspace **+** opens settings for that directory using
its most recently remembered configuration. Locations come from `GET /projects`
in most-recent-first order, keyed by directory and configuration.

Each location shows its path with the final component emphasized and its parent
muted. A configuration label distinguishes named configurations and directories
with several entries. `ui/LocationRow.tsx` separates selection from forgetting.
Normal mode gives the whole row to selection. **Edit** above Recent locations
reveals 44px removal controls in an animated leading gutter; the rest of each row remains selectable,
including the trailing chevron. Choosing a location exits editing. **Done** hides
the controls, and Escape exits editing before cancelling the flow. Closed controls
are inert, disabled and outside the tab order while their gutter slides away.
Reduced-motion preferences disable the transition. Fine-pointer hover uses the
raised surface and strong border tokens of the settings selectors, rather than
changing only the path's text color; touch does not acquire that hover surface.

Removal immediately forgets the index entry without confirmation; it does not
delete files, and a subsequent launch remembers the location again. Local
completion updates filter the latest index state, so overlapping requests cannot
restore an entry another completion already removed.

When the draft's directory/configuration pair is not remembered, it appears in
a separate **Current location** section without a removal control. Forgetting a
selected recent entry therefore removes it from Recents without discarding the
draft or falsely showing it as still remembered. Matching includes configuration,
not just directory, so another remembered configuration cannot hide the current
choice.

`ui/DirectoryDisclosure.tsx` renders **Somewhere else…** as a persistent full-width
row with a down chevron, `aria-expanded` and `aria-controls`. It unfolds a
path input and **Use this directory** below the row without navigating. The input takes an
absolute path or one starting with `~`, which the host reads as its own home directory.
Opening intentionally focuses the input; closing through the row, **Close** or
Escape returns focus to the trigger. The path names a directory on the host, not
necessarily on the browser's device. Choosing or committing a location collapses
the editor. The action pair is right-aligned, with Close before the forward action
Use this directory. The reveal and chevron rotation honor reduced-motion preferences.
Committing a directory moves to settings. Location is the first settings control,
using `ui/SelectionControl.tsx` like the model selectors: the directory and any
named configuration are inside a full-width button with a trailing chevron.
Choosing anywhere on that control returns to location selection; there is no
separate Change action.

## Settings and preview

`ui/NewSessionSettings.tsx` presents main model, effort, temperature, spotter
model, triage model, and worktree. Model controls open the role-specific selection page.
An explicit override has a **Reset** action. Clearing a separate triage selection
returns it to inheritance; the resolved shared state reads **Same as main model**.

The worktree toggle starts the session in its own checkout and branch at
`.efference/worktrees/<id>` (docs/worktrees.md). On it sends `worktree: true`
in the creation body; off it sends nothing, like any other untouched field.
Outside a git repository the host refuses the creation, shown like any other
refusal beside **Start session**.

Effort is a discrete choice strip over low, medium, high, xhigh, and max.
**Use default** clears the override and is separate from that ordinal scale.
Temperature is a number input that commits on blur or Enter; empty clears its
override. Values outside 0–100 are sent rather than silently clamped, allowing
the host to report the issue. Model controls display the effective selection;
preview values never become explicit overrides merely because they were shown.

`ui/sessionForm.ts` converts the draft into one `SessionRequest` for both
`QUERY /sessions` and `POST /sessions`. The browser omits `env`; the host prepares
it through the user-wide environment hook or its own environment
([env.md](env.md)). The form does not claim which hook is running.

Preview requests follow committed changes, not individual temperature
keystrokes. A request counter associates each reply or failure with the draft
it describes and ignores stale answers. Field issues appear beside their
controls; issues without a corresponding control appear beside **Start session**.

The launch action is right-aligned while hints and refusal text remain left-aligned.
Lilac uses the white raised surface for inputs, model controls, and idle effort
choices; dark surfaces and selected effort feedback retain their theme treatment.

Start is disabled without an issue-free preview for the current draft and while
creation is in flight. The latter prevents duplicate submissions: creation is
not idempotent. A refusal keeps the page and edits intact and displays the
host's reason. During creation the button reads **Starting…**, adding elapsed
seconds after five seconds. The host answers once it has reserved the session, so
success opens `/sessions/<id>/` while the session is still being prepared, and a
preparation that fails is shown there ([web-client.md](web-client.md), "A session
that is starting"). Its retry reopens this form on the draft that made the session
(`ui/launchRetry.ts`), with the failure's reason beside **Start session**.

## Model browsing

`ui/ModelPicker.tsx` owns loading and browsing; `ui/pickerRows.ts` builds the
rows; `ui/ModelPickerRow.tsx` separates model selection from model information.
The heading identifies the role being chosen. Breadcrumbs name the connection
and author scope, with a trailing `/` leading into the search input on the next
line. **All models** clears the path and query. Ancestor breadcrumbs truncate
the path and clear the query. The search placeholder and accessible label name
the current namespace, such as **Search in openrouter / deepseek**.

With an empty query:

- The root shows up to five recent picks for the main role, then configured
  connections. Spotter and triage show connections without main-role recents.
- A connection with author categories shows main-role recent picks, alphabetic
  author folders, and any ungrouped models. It does not repeat every author's
  models below the folders. Section labels distinguish recents, authors, and
  ungrouped models.
- A connection without author categories shows its models.
- An author page shows that author's models.

Typing searches the current namespace, including author descendants when the
scope is a connection. Matching is `ui/matcher.ts`, pinned to the terminal
matcher's `src/clients/tui/session_form/match_cases.json`. A typed-ID row remains
available alongside catalog matches; under an author it completes the ID with
that author's prefix. Render keys include row kind as well as slug, because a
typed-ID action and a catalog row can share a slug while doing different things.

Folder rows show model counts and a descent chevron, plus unavailable or failed
status where applicable. They do not show a connection's wire dialect. Model
rows use the catalog's display name, falling back to an identifier if none is
supplied. At the root, a connection badge distinguishes the same model name
served by different connections. Within a namespace the breadcrumbs establish
the source, so model rows do not repeat the connection or an identifier beneath
a supplied display name.

Choosing the model row commits its connection and provider-native model ID and
returns to settings in one action. A separate right-aligned information button
has a 44-pixel hit target, `aria-expanded`, and `aria-controls`. It expands a
labelled panel without choosing the model. The panel contains the full
identifier, author when declared, exact context size, separate input/output
prices per million tokens, model age, declared reasoning/image support, and
recent-pick status. Undeclared facts are omitted; nonpositive creation timestamps
do not produce ages. Optional catalog descriptions are shown in full beneath
the parameters. The host supplies provider descriptions through the catalog
([model-catalog.md](model-catalog.md)).

Search autofocus is limited to hover-capable fine pointers. On devices outside
that media condition, search waits for intentional focus. Arrow keys activate keyboard
highlighting and move its index; Enter chooses the indexed row. Pointer
interaction or scrolling clears the highlight. Hover feedback is restricted to
hover-capable fine pointers. A connection badge uses the panel background inside
a highlighted row so it remains distinct. Inputs and selection-field values at the ≤700px breakpoint are at least 16px.
The input size avoids iOS focus zoom without disabling user zoom; matching
selector values prevents a larger-looking temperature placeholder.

Escape clears a nonempty search, then removes a namespace level, then returns
to settings. Backspace on an empty search removes the final namespace level.
Information-button keyboard activation is not intercepted as model selection.
Outside the picker, Escape cancels the launch flow.

## Catalog loading

Opening reads `GET /inference/catalog?cwd&config`, which serves stored copies
without waiting on providers ([model-catalog.md](model-catalog.md)). Connections
without a usable copy get an awaited refresh, replacing only their own rows.

After the stored catalog has loaded, a one-word query that matches no catalog
row at connection scope can request one `max_age=60` refresh of that connection
per visit. Restoring a saved query does not refresh merely because the stored
catalog has not yet arrived. The age clock is captured once per picker visit.
