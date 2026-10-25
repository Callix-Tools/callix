---
sidebar_position: 4
---

# Command line

`callix` is the same analysis as the library, as a program: point it at a
directory, say what you want back. It finds the languages in the tree by itself,
runs every adapter that has something to analyse, merges the graphs, and writes
the graph, a file in another format, or a report.

```bash
callix analyze .                        # the graph, as JSON, on stdout
callix export . -f lsif -o dump.lsif    # an LSIF index
callix export . -f cyclonedx            # an SBOM of the declared dependencies
callix report . -t status               # how complete symbol resolution was
```

The binary is native: the same adapters and resolvers as the library, built
without the Python bindings, with ty and typescript-go inside. It links nothing
but libc and needs no Python to run. Get it as

- a tarball from the GitHub Release — Linux (x86_64, aarch64) and macOS (Apple
  Silicon, Intel), each with a `.sha256`;
- the [Docker image](./docker.md), which also carries the Go and Rust toolchains
  that resolving those languages needs;
- `task cli` from a checkout (Rust ≥ 1.95, Go ≥ 1.26, the same as the library).

Go and Rust projects are resolved with the toolchains already on the machine, as
with the library — `callix doctor` says which are there.

## Commands

| Command | What it does |
|---|---|
| `callix analyze DIR` | Analyse a directory and write the graph as JSON. |
| `callix export INPUT -f FORMAT` | Write the graph in another [format](./export-formats.md). |
| `callix report INPUT -t TYPE` | Print a report: `summary`, `status`, `dependencies`, `boundaries` or `calls`. |
| `callix diff OLD NEW` | Compare two saved graphs. |
| `callix merge A B …` | Merge saved graphs — the cross-language step. |
| `callix formats` | List the formats, report types and languages. |
| `callix doctor` | Check which toolchains are present, and what each one enables. |

`INPUT` is a project directory (it is analysed first), a graph JSON file saved
earlier, or `-` for stdin. A saved graph exports without analysing anything
again, which is the point of saving one:

```bash
callix analyze . -o graph.json
callix export graph.json -f cyclonedx -o sbom.json
callix export graph.json -f lsif --source-root . -o dump.lsif
```

## Options that shape the analysis

| Option | |
|---|---|
| `-l, --lang LANG` | Languages to analyse, comma-separated: `python`, `typescript`, `go`, `rust`, `php`, `c`, `cpp`, `yaml`. By default, whatever the tree contains. `ts`, `js`, `golang`, `c++` and the like are accepted. |
| `--no-resolve` | A structural graph: fast, no `CALLS` or `REFERENCES`. |
| `--strict` | Exit 3 unless every language resolved fully. Writes nothing when it refuses. |
| `--prepare` | Fetch the project's dependencies first — see below. |
| `--source-root DIR` | Where the sources are, when `INPUT` is a saved graph. |
| `-q, --quiet` | Print only the requested output and errors. |

A language that fails during detection is a warning; one you named with `--lang`
that fails is an error. Where several languages share a tree their graphs are
merged with `allow_shared`, so a boundary seen from both sides becomes one node.

### `--strict` and what it can promise

`--strict` refuses anything not fully resolved, and PHP, C and C++ are resolved
by a symbol table that says `degraded` on purpose. So `--strict` on a tree with
any of them always exits 3. Use `-l` to narrow it to the languages you want to
vouch for:

```bash
callix analyze . --strict -l python,typescript,go,rust
```

### `--prepare`

A resolver can only answer for code that is on disk. In a fresh container there
is none, so `--prepare` runs what each manifest calls for before analysing:

| Found | Runs |
|---|---|
| `go.mod` | `go mod download` |
| `Cargo.toml` | `cargo fetch` |
| `package.json` without `node_modules` | `pnpm install --frozen-lockfile`, `yarn install --frozen-lockfile`, `npm ci` or `npm install`, by lockfile, always with `--ignore-scripts` |

Manifests are searched four levels deep, skipping `node_modules`, `vendor`,
`target`, `.venv` and the like. A step that fails or whose tool is missing is a
warning: it costs resolution quality, never the run.

Python is deliberately left out. Installing a project's packages means running
its build backend, which a code analyser should not do on its own. Install them
into the environment the analysis runs in — in the image, `pip install -r
requirements.txt` — and ty finds them.

## Reports

```bash
callix report . -t summary              # counts, languages, the largest files
callix report . -t status -f md         # resolver status, per language
callix report . -t calls --symbol pkg.service.build
callix report graph.json -t boundaries -f json
```

`-f` picks the style: `text` (default), `md` or `json`. `status` is the one to
read before trusting a graph: it says whether resolution ran, how many queries
resolved, and what each language's backend can promise.

## Exit codes

| Code | |
|---|---|
| 0 | Success. |
| 1 | The analysis or an I/O step failed; also `diff --exit-code` when the graphs differ. |
| 2 | A usage error (clap's). |
| 3 | `--strict` refused the graph. |

Warnings go to stderr. `callix export … --deny-warnings` turns any warning an
exporter raises into a failure, for pipelines that want a clean index or none.
