# Libraries built in for artifacts

The browser builds an artifact loads with `efference.load(name)` (docs/artifacts.md), one
version each, embedded in the binary by `src/listeners/artifacts/libraries.rs` (`LIBRARIES`). The
versions are deliberately not the newest: each is a major version models know well, so
an agent writes against an API it has seen. The artifacts guide
(`assets/guide/artifacts.md`) names them; a test checks it lists exactly these.

| `load` name | package | version | file | npm integrity |
| --- | --- | --- | --- | --- |
| `d3` | d3 | 7.9.0 | `d3/d3.min.js` | `sha512-e1U46jVP+w7Iut8Jt8ri1YsPOvFpg46k+K8TpCb0P+zjCkjkPnV7WzfDJzMHy1LnA+wj5pLT1wjO901gLXeEhA==` |
| `vega-embed` | vega | 5.33.1 | `vega/vega.min.js` | `sha512-1fArfVDUqVbb5z8n+/nVQb+VNBzx8svY6CrsyTK4Ujm4yrd9I1auoKxBAPXLCavY1LcrbHUskQbGUP8gXOzyQw==` |
| | vega-lite | 5.23.0 | `vega-lite/vega-lite.min.js` | `sha512-l4J6+AWE3DIjvovEoHl2LdtCUkfm4zs8Xxx7INwZEAv+XVb6kR6vIN1gt3t2gN2gs/y4DYTs/RPoTeYAuEg6mA==` |
| | vega-embed | 6.29.0 | `vega-embed/vega-embed.min.js` | `sha512-PmlshTLtLFLgWtF/b23T1OwX53AugJ9RZ3qPE2c01VFAbgt3/GSNI/etzA/GzdrkceXFma+FDHNXUppKuM0U6Q==` |
| `chart.js` | chart.js | 4.4.9 | `chart.js/chart.umd.js` | `sha512-EyZ9wWKgpAU0fLJ43YAEIF8sr5F2W3LqbS40ZJyHIner2lY14ufqv2VMp69MAiZ2rpwxEUxEhIH/0U3xyRynxg==` |
| `mermaid` | mermaid | 11.4.1 | `mermaid/mermaid.min.js` | `sha512-Mb01JT/x6CKDWaxigwfZYuYmDZ6xtrNwNlidKZwkSrDaY9n90tdrJTV5Umk+wP1fZscGptmKFXHsXMDEVZ+Q6A==` |

Each file is the package's published UMD build (Mermaid's is `dist/mermaid.min.js`, an IIFE assigning `globalThis.mermaid`), unmodified — the host drops its
`sourceMappingURL` comment when serving it, as the maps are not vendored — taken from the tarball
`npm pack <package>@<version>` downloads; the integrity column is that tarball's, as
`npm view <package>@<version> dist.integrity` reports it. Licences sit beside each build.
Mermaid also draws every `mermaid` fence (`web/public/artifacts/mermaid.js`), whose theme
variables are named for this version's themes. Its build is 2.57 MB; the newest 11.x when
it was vendored, 11.17.2, is 3.57 MB.
The vega tarball ships no licence file: `vega/LICENSE` is vega-lite's, whose holder and
BSD-3-Clause terms vega's `package.json` and repository share.

To change a version: fetch the tarball the same way, replace the file and the row here,
and update `LIBRARIES`, the guide's list, and (for Mermaid) the session preamble together. Upgrading changes how every artifact
renders, including those in old conversations.
