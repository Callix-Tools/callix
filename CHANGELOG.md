# Changelog

All notable changes to this project will be documented in this file.

## [1.6.0] - 2026-10-26
### Features

- **python**: feat(python): resolve a project inside the directory, and keep what the authors wrote by @Neko1313 in [#11](https://github.com/Callix-Tools/callix/pull/11)




**Full Changelog**: https://github.com/Callix-Tools/callix/compare/v1.5.0...v1.6.0
## [1.5.0] - 2026-10-26
### Features

- feat: close the accuracy suites for every language (Python, PHP, C/C++, Go, Rust, TypeScript) by @Neko1313 in [#10](https://github.com/Callix-Tools/callix/pull/10)




**Full Changelog**: https://github.com/Callix-Tools/callix/compare/v1.4.0...v1.5.0
## [1.4.0] - 2026-10-26
### Features

- **resolution**: Measure resolution per language and say truthfully how each edge was found by @Neko1313 in [#6](https://github.com/Callix-Tools/callix/pull/6)
- **flow**: feat: follow the iteration protocol, calls through parameters and C/C++ callbacks by @Neko1313 in [#7](https://github.com/Callix-Tools/callix/pull/7)
- **python**: feat(python): a lambda is a function node by @Neko1313 in [#8](https://github.com/Callix-Tools/callix/pull/8)
- **python**: feat(python): follow what a loop hands out by @Neko1313 in [#9](https://github.com/Callix-Tools/callix/pull/9)


### New Contributors

- @Neko1313 made their first contribution in [#9](https://github.com/Callix-Tools/callix/pull/9)



**Full Changelog**: https://github.com/Callix-Tools/callix/compare/v1.3.0...v1.4.0
## [1.3.0] - 2026-10-26
### CI/CD

- verify the published CLI tarballs on every platform by @claude

### Features

- **python**: follow routers across modules, bind imports whatever the file order, resolve imports to their definition by @claude
- feat: cross-module routers, order-independent imports, documentation in the graph; baselines; verify-release by @claude in [#5](https://github.com/Callix-Tools/callix/pull/5)

### Testing

- **baseline**: recapture zod and superset after the walk reaches decorators, defaults and receivers by @claude
- **baseline**: recapture redis, leveldb and rocksdb by @claude




**Full Changelog**: https://github.com/Callix-Tools/callix/compare/v1.2.0...v1.3.0
## [1.2.0] - 2026-10-26
### Bug Fixes

- **ci**: fix(ci): keep image digests out of the release assets, add a resume workflow by @claude in [#2](https://github.com/Callix-Tools/callix/pull/2)
- **export**: file a reference under the file it was written in by @claude
- **boundaries**: keep the pyfunction attributes on normalize_http_path by @claude
- **rust**: file an attribute route under the function it is written on by @claude

### CI/CD

- **release**: ci(release): pull the published image tags back from ghcr and smoke-test them by @claude in [#3](https://github.com/Callix-Tools/callix/pull/3)
- **docker**: run the images and every binary on real projects by @claude
- **docker**: compare dump paths against the physical work directory by @claude

### Documentation

- describe the file key on edge metadata by @claude
- fix: file references and routes under the right node; walk decorators, defaults and call receivers by @claude in [#4](https://github.com/Callix-Tools/callix/pull/4)

### Features

- **python,typescript**: walk decorators, defaults and call receivers; file routes under their handler by @claude

### Styling

- **cfamily**: pass the occurrence to push_reference instead of its parts by @claude

### Testing

- **golden**: the TypeScript fixture gains the references it used to lose by @claude




**Full Changelog**: https://github.com/Callix-Tools/callix/compare/v1.1.0...v1.2.0
## [1.1.0] - 2026-10-26
### Bug Fixes

- **cfamily**: record the use-site span on resolved edges by @claude
- **lint**: allow clone_on_copy for PyO3's from_py_object expansion by @claude
- **docker**: clean up the smoke test's root-owned workspace through the image by @claude

### Build System

- Debian images, standalone CLI binaries and CI checks for the native binary by @claude

### CI/CD

- run the Rust unit tests
- build both image architectures and the binary tarballs on every PR by @claude
- **release**: feat: callix CLI, native engine, exporters (LSIF, SCIP, CycloneDX…) and Docker images by @claude in [#1](https://github.com/Callix-Tools/callix/pull/1)

### Documentation

- bring the docs up to 1.0.0, and correct what drifted
- **bench**: refresh benchmark metrics [skip ci] by @github-actions[bot]

### Features

- add the callix CLI and the callix-export crate by @claude
- add Docker images, release jobs and documentation for the CLI by @claude
- build the engine without Python and run the CLI on it by @claude
- add the SCIP exporter by @claude

### Testing

- extend benchmarks and baselines to all eight languages


### New Contributors

- @claude made their first contribution in [#1](https://github.com/Callix-Tools/callix/pull/1)



**Full Changelog**: https://github.com/Callix-Tools/callix/compare/v1.0.0...v1.1.0
## [1.0.0] - 2026-07-30
### Bug Fixes

- **ci**: actually run the benchmark matrix on a manual dispatch
- **bench**: stop understating peak memory outside a container
- **ci**: stop stale results overwriting fresh ones when artifacts merge
- store relative file paths on every node, not just FILE
- keep vendored and built trees out of the graph
- make Graph.subgraph deterministic
- one structural edge per pair, not one per occurrence
- **ci**: pin setup-uv exactly — its major tag stops at v7

### CI/CD

- move off the node20 action runtime
- update every action, and make the docs build runnable locally

### Documentation

- **bench**: refresh benchmark metrics [skip ci] by @github-actions[bot]
- **bench**: refresh benchmark metrics [skip ci] by @github-actions[bot]
- **bench**: refresh benchmark metrics [skip ci] by @github-actions[bot]
- **bench**: refresh benchmark metrics [skip ci] by @github-actions[bot]
- **bench**: refresh benchmark metrics [skip ci] by @github-actions[bot]
- add the Docusaurus documentation site
- link the documentation site from the README
- slim the README down and split out CONTRIBUTING
- align the package description with the repository one
- state what 1.0 promises, and what it cannot
- bring the whole site up to eight languages, and test the examples
- say which languages the published release actually has

### Features

- **rust**: emit parameters, fields, imports, references and types
- **go**: emit parameters, fields, imports and type annotations
- fill in the dead vocabulary — DEPENDENCY, DEPENDS_ON, COMMUNICATES_WITH
- accept custom boundary extractors, and document the real coverage
- detect boundaries by argument shape rather than receiver name
- lower the Python floor to 3.10 and publish musl wheels
- add the YAML adapter — specifications as boundaries
- **yaml**: follow includes, read CI pipelines, recognize Helm
- add the PHP adapter
- add the C and C++ adapters
- give every adapter the same constructor
> **BREAKING CHANGE**: `PythonAdapter`'s `dep_parsers` and `resolver` are now
keyword-only, and follow `resolve` in the signature. Every documented use
was already by keyword.

### Miscellaneous

- **deps**: update the embedded ty from 0.0.52 to 0.0.65

### Testing

- add golden fixture tests, cross-validated against graphlens
- fingerprint the eight benchmark projects, and record the graphlens delta
- cover the API that 1.0 freezes, and fix what it found
- make the baselines measure the resolvers, not just the visitors




**Full Changelog**: https://github.com/Callix-Tools/callix/compare/v0.1.0...v1.0.0
## [0.1.0] - 2026-07-29
### Bug Fixes

- ci
- ci release
- **build**: unbreak the linux wheel and widen the platform matrix

### Documentation

- **bench**: refresh benchmark metrics [skip ci] by @github-actions[bot]
- add CLAUDE.md

### Features

- reinit
- update docs


### New Contributors

- @github-actions[bot] made their first contribution
- @ made their first contribution


<!-- generated by git-cliff -->
