# efference

An agentic coding harness. A long-lived host process holds every session and runs its turn
loop against Anthropic Messages (Anthropic, OpenRouter) or OpenAI Responses (the Codex
backend and the platform API). The terminal client, the web client, the CLI verbs and other
agents are all clients of the host's HTTP control channel, so a session keeps running when
its client detaches. Each session is an append-only log, and the transcript sent to the
model is projected from it every turn.

The manual is at [efference.net](https://efference.net).

## Install

```sh
curl -fsSL https://efference.net/install.sh | sh
```

macOS and Linux, on x86_64 and aarch64.

## This repository

The source is published here for reading and audit. It is generated from a development
repository by leaving out the paths that stay private (session notes and planned work), so
its commit hashes differ from the development repository's, and some links in `docs/` name
files that are not here. Pull requests are read; one that is accepted is applied in the
development repository and arrives here with the next publish.

[docs/README.md](docs/README.md) is the top of the architecture documentation.

## Building

Stable Rust, and pnpm with Node for the web client, which the build compiles into the
binary. The syntax themes are submodules.

```sh
git clone --recurse-submodules https://github.com/jbr/efference
cd efference
cargo build --release --no-default-features
```

`--no-default-features` matches a release build, which leaves out the `efference dev`
verbs.

## Verifying a release

Every file under `efference.net/releases/` has a [minisign](https://jedisct1.github.io/minisign/)
signature beside it, made with the key whose public half release binaries carry to check
their own updates:

```sh
minisign -Vm efference-aarch64-apple-darwin.tar.xz \
  -P RWQVdWuS5d3d6sqA6eXxVuPYvV7/DdkJDgAyDTLmYd3Lz7yssgny/Vq0
```

The signature's trusted comment, which minisign prints, names the version and the file:
`efference <tag> <file name>`.

Releases built in this repository also carry a GitHub build-provenance attestation for each
archive, naming the commit and workflow run that built it:

```sh
gh attestation verify efference-aarch64-apple-darwin.tar.xz --repo jbr/efference
```

## License

MIT OR Apache-2.0, at your option.
