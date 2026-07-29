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
| `openapi:` or `swagger:` | one `EXPOSES` per path × verb under `paths:`, confidence `1.0` |
| `apiVersion:` + `kind: Ingress` | one `EXPOSES` per `spec.rules[].http.paths[].path`, method `ANY` |
| `services:` | a `MODULE` per service, `DEPENDS_ON` between them from `depends_on` |

Compose's `depends_on` is accepted in both spellings — a plain list, and the
mapping form whose values carry conditions.

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

CI configuration files, Helm templates and anything else built from placeholders
are parsed but produce nothing. A Helm chart is not valid YAML until it is
rendered; run the adapter on rendered output if you need it.
