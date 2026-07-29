---
sidebar_position: 7
---

# YAML adapter

```python
from callix import YamlAdapter

graph = YamlAdapter().analyze("path/to/repo")
```

| Property | Value |
|---|---|
| Language id | `yaml` |
| Project marker | any `.yaml` / `.yml` file |
| Extensions | `.yaml`, `.yml` |
| Resolver | none — there are no symbols |
| External requirements | none |

## Why YAML is here

YAML is not a programming language and this adapter does not pretend it is:
there are no functions to declare and nothing to resolve. What it contributes is
the half of a system that code never states.

An OpenAPI document lists the routes a service serves. A Kubernetes Ingress
lists the paths that reach the cluster. A Compose file lists the services and
which of them wait on which. None of that appears in any source file, and all of
it is part of how the system fits together.

Because a boundary's ID comes from the mechanism and the normalized key alone,
a route declared in a specification lands on **the same node** as the handler
that implements it and the client that calls it:

```python
from callix import PythonAdapter, YamlAdapter

graph = YamlAdapter().analyze("api")            # openapi.yaml
graph.merge(PythonAdapter().analyze("api"), allow_shared=True)
graph.link_boundaries()
# client.fetch_one -> openapi.yaml   through GET /engines/{}
```

That is what makes drift between a specification and its implementation a
question the graph can answer: a route in the spec with nothing exposing it, or
a handler with no spec entry, both show up as a boundary with only one side.

## What it extracts

The flavour is read off the top-level keys, since there is no manifest to
consult:

| Recognized by | Produces |
|---|---|
| `openapi:` or `swagger:` | one `EXPOSES` per path × verb under `paths:`, confidence `1.0`; external `$ref` targets as `IMPORTS` |
| `apiVersion:` + `kind: Ingress` | one `EXPOSES` per `spec.rules[].http.paths[].path`, method `ANY` |
| `services:` | a `MODULE` per service, `DEPENDS_ON` between them from `depends_on` |
| `.gitlab-ci.yml` | a `MODULE` per job, `DEPENDS_ON` from `needs`, `IMPORTS` from `include` |
| `.github/workflows/*.yml` | a `MODULE` per job, `DEPENDS_ON` from `needs`, a `DEPENDENCY` per external `uses:`, `IMPORTS` for a local one |
| `Chart.yaml` | a `DEPENDENCY` per subchart under `dependencies:` |
| `kustomization.yaml` | `IMPORTS` per entry under `resources:` |

Both spellings are accepted wherever YAML allows two — a dashed list and a flow
sequence, a plain list and a mapping whose values carry conditions. That is not
politeness: `needs: [build]` and a dashed `needs:` appear in the same file
routinely.

## Multiple files

A reference to another document becomes an `IMPORTS` edge, so multi-file YAML
stops being a coincidence of two files sharing a directory:

```
compose.yaml       --imports-->  compose.db.yaml            (file)
.gitlab-ci.yml     --imports-->  ci/build.yml               (file)
.gitlab-ci.yml     --imports-->  Security/SAST.gitlab-ci.yml (external symbol)
workflow:verify    --imports-->  ./.github/workflows/reusable.yml
```

Paths are resolved relative to the *including* file, and a target that is not a
local file — a GitLab `template:` or `remote:`, an `http` `$ref` — becomes an
`EXTERNAL_SYMBOL` rather than being dropped, the same rule the other adapters
follow for imports.

Job names are qualified by their file (`.github/workflows/ci.yml:test`): two
pipelines may both have a `test`, and they are not the same job. Compose service
names are not, because a service is global to its project.

## Helm

Point the adapter at a chart directory and it will read `Chart.yaml` — the
subcharts under `dependencies:` become `DEPENDENCY` nodes — but it will get
nothing out of `templates/`.

That is not an omission. A Helm template is not YAML:

```yaml
path: {{ .Values.ingress.path | quote }}
```

Tree-sitter parses it into something, and the something is meaningless — the key
of that boundary would literally be `{{ .Values.ingress.path | quote }}`. So any
document containing `{{ … }}` is marked `helm-template` and skipped, which is
more useful than a graph full of unrendered placeholders.

For the real thing, render first:

```bash
helm template . --output-dir rendered
```

```python
YamlAdapter().analyze("rendered")   # http:ANY /engines
```

The rendered manifests are ordinary Kubernetes YAML, and everything above
applies to them.

A specification states its contract outright, so nothing about it is inferred
and the confidence is `1.0`. An Ingress says nothing about the method, so the
key records it as `ANY` — the same shape Go's own `ServeMux` produces.

## What it does not do

No `FUNCTION`, `CLASS` or `METHOD` nodes: YAML has none. Boundaries attach to
the `FILE` node, which is the innermost thing that contains them.

`resolver_status` is `ok` with zero queries. That is deliberate and different
from `unavailable`: nothing failed and nothing is missing, because there was
nothing to resolve. `resolve=False` is accepted and ignored, so the adapter
interface stays uniform.

Every file carries its recognized flavour in `metadata["flavour"]`, so a
document callix could make nothing of is visible as `unknown` rather than
silently absent.
