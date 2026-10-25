---
sidebar_position: 6
---

# Docker

The [command line](./cli.md) ships as two images, for `linux/amd64` and
`linux/arm64`:

| Image | Contains | Resolves |
|---|---|---|
| `ghcr.io/callix-tools/callix:full` (also `:latest`) | `callix`, Python, **Go**, **Rust** with `rust-analyzer` and the standard library's sources, Node and npm | Python, TypeScript, Go, Rust — fully |
| `ghcr.io/callix-tools/callix:slim` | `callix` and Python | Python and TypeScript fully; Go and Rust come back structural |

Both resolve PHP, C and C++ through the symbol table and report `degraded`, and
YAML declares no symbols. No image changes that: it is what those resolvers are,
not something missing from the container.

Tags: `1.2.3`, `1.2`, `1` and `latest` for `full`; `1.2.3-slim`, `1.2-slim`,
`1-slim` and `slim` for the other. The versions of Go and Rust inside are pinned
per release, because what a resolver answers depends on its version.

```bash
docker run --rm -v "$PWD:/workspace" ghcr.io/callix-tools/callix:full \
  export . -f lsif -o dump.lsif
```

The image's entrypoint is `callix`, so everything after the image name is a
`callix` argument. The working directory is `/workspace`.

## "Without degradation" — what the image guarantees

The image brings the **tools**. It cannot bring your project's **dependencies**,
and a resolver can only answer for code it can read:

| Language | The image has | The project must have |
|---|---|---|
| Python | ty, linked in; Python 3.13 | its packages installed in the container's Python (`pip install -r requirements.txt`) |
| TypeScript | typescript-go, linked in | `node_modules` (`npm ci`, or `--prepare`) |
| Go | the toolchain | the module cache (`go mod download`, or `--prepare`) |
| Rust | cargo, rust-analyzer, `rust-src` | the registry sources (`cargo fetch`, or `--prepare`) |

Without them the analysis still succeeds, and standard-library and in-repo
symbols resolve — third-party ones fall through to external symbols.
`callix report . -t status` shows how much resolved, and `--strict` turns "less
than fully" into a failing exit code.

A project with a `rust-toolchain.toml` pinning another toolchain makes `rustup`
fetch it, which needs the network.

## GitLab code navigation

GitLab reads an LSIF report from a job's artifacts:

```yaml
code_navigation:
  stage: test
  image:
    name: ghcr.io/callix-tools/callix:full
    # The image's entrypoint is `callix`; GitLab needs a shell there.
    entrypoint: [""]
  script:
    - callix export . -f lsif --prepare -o dump.lsif
  artifacts:
    reports:
      lsif: dump.lsif
```

For a Python project add `pip install -r requirements.txt` before the export; for
TypeScript, run `npm ci` in an earlier job and pass `node_modules` along.
Check GitLab's documentation for the current size limits on LSIF reports.

## GitHub Actions

```yaml
jobs:
  sbom:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
      - run: |
          docker run --rm -v "$PWD:/workspace" ghcr.io/callix-tools/callix:full \
            export . -f cyclonedx -o sbom.cdx.json
      - uses: actions/upload-artifact@v7
        with: { name: sbom, path: sbom.cdx.json }
```

## Running as another user

The image runs as root. Under `--user`, point the caches somewhere writable —
`GOCACHE`, `GOPATH` and `CARGO_HOME` are set for root's layout — and note that
`git` is configured to trust any directory, so a workspace owned by a different
uid is not refused.

## Building it

```bash
task docker            # the full image, as callix:dev
task docker:slim       # the slim image, as callix:dev-slim
task docker:smoke      # run the smoke test against callix:dev
```

The binary links `libpython3.X`, so both images are built from the same
`python:3.X` Debian base and `PYTHON_VERSION` is one argument for the file. A
cold build compiles ty and typescript-go and takes a while; the BuildKit cache
mounts keep the compiled tree between local builds.
