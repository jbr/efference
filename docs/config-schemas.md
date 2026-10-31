# Config schemas

efference publishes a JSON schema for each of its two config files, so an editor running a
TOML language server (Taplo, which Even Better TOML and most editors' TOML support use, or
Tombi) completes keys, describes each on hover, and flags a misspelled key before efference
reads the file.

| Schema | Describes | Generated from |
| --- | --- | --- |
| `user.v1.json` | `~/.efference/config.toml` | `InferenceConfig` (`src/user_config.rs`) |
| `project.v1.json` | a project's `efference.toml`, and each session's `config.toml`, which starts as a copy of it | `RawConfig` (`src/config/raw.rs`) |

Both are served from `https://efference.net/schemas/`, and a config file names one with a
first line of `#:schema <url>`. `src/config_schemas.rs` generates them; they are committed
under `assets/schemas/`.

## Generation

The types the files deserialize into derive `schemars::JsonSchema`, so the schema follows the
parser: `deny_unknown_fields` becomes `additionalProperties: false`, a serde rename becomes the
property name, an internally tagged enum (`auth = { kind = … }`) becomes a `oneOf` keyed on its
tag. A type whose `Deserialize` is hand-written implements `JsonSchema` by hand to match
(`EnvEntry`), or derives it with `#[schemars(untagged)]` describing the spellings its
deserializer accepts (`RawContextValue`, `RawPathValue`).

`tidy` then rewrites what schemars emits for TOML editors, at every depth:

- TOML has no null, so an `Option` field loses the `null` in its type and the `default: null`
  its `#[serde(default)]` produces.
- A numeric `format` (`uint64`, `double`) is a Rust width rather than a format either editor
  checks, and is dropped.
- A derived enum whose variants carry docs comes out as a `oneOf` of `const`s. It becomes a
  plain `enum`, with the variant docs in `x-taplo.docs.enumValues`, matched by index. As a
  `oneOf`, a bad value reads as one failure per variant in Tombi and Taplo prints every
  variant's doc on hover.

A closed vocabulary the config holds as a string takes its `enum` from the table the parser
reads, through `#[schemars(schema_with)]` and `config_schemas::documented_enum`: `provider`
from `Provider::NAMED`, `dialect` from `WireDialect::NAMED`, `builtin` from the guide's
`DOCUMENTS`. The same tables produce the parser's "valid values are" error, so the schema, the
parser, and its error cannot disagree. `output-cap-field`'s two spellings sit beside the
`match` in `RequestOptions::new`.

What the schema does not express stays the parser's alone: mutually exclusive keys (`provider`
or `dialect`; `path`, `exec`, or `builtin`), a `[commands]` key that is not one simple command,
a `sort` spec's grammar.

## Hover text

On a type that reaches a schema, a `///` comment is published: schemars copies it into
`description`, and it is what someone editing the file reads on hover. It says what the key
means, its valid values, and its default, in terms of the file rather than the code. Notes for
implementers on the same items are `//` comments (process/comments.md).

Two editor behaviors decide where a description goes:

- **Taplo ignores every keyword beside a `$ref`.** A field whose type is a struct (`[bash]`,
  `[host]`) or a documented enum (`images`, `auth`) carries no `///`; its type's doc is the
  hover text, in both editors.
- **Taplo prints the description of every `anyOf` arm, one after another.** A value spelled
  `true` or a table (`[context]`, `[paths]`, `[env]` entries) carries its description on the
  union, and its table arm carries none.

Tombi reads no per-value docs, so where each value's meaning matters (`[commands]` stances), the
value's description states it in prose as well.

## Versions

The pin test, `committed_schemas_match_the_generator`, fails when a config type changes and the
committed schema does not; `EFFERENCE_WRITE_SCHEMAS=1 cargo test config_schemas` regenerates
them. A new optional key is published under the same file name. A key renamed or removed moves
the schema to the next version, and the previous file stays served, since config files in the
wild name it.

## The `#:schema` line

Taplo and Tombi read the directive from the top of the document. It is a comment, not a key:
`#:schema <url or path>`, with no `=` and no quotes; Tombi reads `= "…"` as part of a relative
path and reports only a warning that the file is missing. efference writes it:

- `efference setup`, on a fresh `~/.efference/config.toml` (`src/setup/render.rs`). An existing
  file gains none.
- `CONFIG_SCAFFOLD`, the config a session gets when the project has none
  (`src/boot/scaffold.rs`).
- The `guide/new-project` template, which a setup session follows when writing a project's
  `efference.toml`. The guide has the session ask the operator first, since the editor
  fetches the schema from efference.net and so tells it when the file is edited; declining
  leaves the line out, and nothing else reads it.

A session's snapshot copies the project file verbatim, line included. Both config writers —
a session's config edits (`ConfigFile::apply`) and setup's render — pass their output through
`config_schemas::keep_directives_first`: toml_edit keeps a leading comment as decor on the first
item, so an edit that renders a new item ahead of it, such as a top-level `tools` before the
first table, would otherwise carry the line down the file, where editors no longer read it.

## Publication

The efference.net site repository copies `assets/schemas/` into its build at `/schemas/`, the
same way it reads the manual from `manual/` (that repository's `docs/site.md`). Only that
directory is published; the rest of `assets/` is embedded in the binary.
