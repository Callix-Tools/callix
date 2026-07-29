# callix documentation site

This directory contains the [Docusaurus](https://docusaurus.io/) site published
to <https://callix-tools.github.io/callix/>. The documentation source lives in
[`docs/`](./docs) as Markdown; the rest of this directory is the Docusaurus
project (it is isolated from the Rust workspace and has its own toolchain).

## Prerequisites

- [Node.js](https://nodejs.org/) **≥ 20**
- [pnpm](https://pnpm.io/) **10** (`corepack enable` will install it)

## Local development

```bash
cd website
pnpm install
pnpm start        # http://localhost:3000/callix/ with hot reload
```

## Build and preview the production site

```bash
pnpm build        # output written to website/build
pnpm serve        # serve the built site locally
```

`pnpm build` is the same command CI runs, and it fails on broken internal
links — run it before opening a docs PR.

## Type-check

```bash
pnpm typecheck
```

## Deployment

Pushes to `main` that touch this directory trigger
[`.github/workflows/docs.yml`](../.github/workflows/docs.yml), which builds the
site and publishes it to GitHub Pages. See
[CI Integration → docs](./docs/ci-integration/overview.md) for details.

## Where to add content

1. Add a Markdown file under the relevant `docs/<category>/` folder.
2. Register it in [`sidebars.ts`](./sidebars.ts).
3. Run `pnpm build` to confirm there are no broken links.
