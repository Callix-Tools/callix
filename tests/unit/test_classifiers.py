"""`ImportClassifier`, `TsImportClassifier` and the Go/Rust classify functions.

The result lands in `Node.metadata["origin"]` and is what tells a first-party
import from a dependency in every consumer of the graph. The four languages
answer with the same four words but key on different things, which is the part
worth pinning.
"""

from __future__ import annotations

import pytest

from callix import ImportClassifier, TsImportClassifier, classify_go_import, classify_rust_import

ORIGINS = {"stdlib", "internal", "third_party", "unknown"}


def test_python_classifier_keys_on_the_top_level_name():
    classifier = ImportClassifier(stdlib={"os"}, third_party={"httpx"}, internal={"pkg"})
    assert classifier.classify("os") == "stdlib"
    assert classifier.classify("httpx") == "third_party"
    assert classifier.classify("pkg") == "internal"
    # "In none of the sets" — a transitive dependency, or nothing at all.
    assert classifier.classify("mystery") == "unknown"


def test_python_classifier_without_any_sets_answers_unknown():
    assert ImportClassifier().classify("os") == "unknown"


def test_python_classifier_repr_reports_set_sizes():
    classifier = ImportClassifier(stdlib={"os", "sys"}, third_party={"httpx"})
    assert repr(classifier) == "ImportClassifier(stdlib=2, third_party=1, internal=0)"


@pytest.mark.parametrize(
    ("specifier", "origin"),
    [
        ("node:fs", "stdlib"),
        ("lodash", "third_party"),
        # The key is the package, so a deep import classifies like its package.
        ("lodash/fp", "third_party"),
        ("@scope/pkg", "third_party"),
        ("@scope/pkg/sub", "third_party"),
        ("src", "internal"),
        ("src/a/b", "internal"),
        ("mystery", "unknown"),
        # A relative specifier is not a package at all.
        ("./sibling", "unknown"),
        ("../parent", "unknown"),
    ],
)
def test_typescript_classifier_keys_on_the_package_name(specifier, origin):
    classifier = TsImportClassifier(
        stdlib={"node:fs"}, third_party={"@scope/pkg", "lodash"}, internal={"src"}
    )
    assert classifier.classify(specifier) == origin


def test_typescript_classifier_repr_reports_set_sizes():
    assert repr(TsImportClassifier()) == "TsImportClassifier(stdlib=0, third_party=0, internal=0)"


@pytest.mark.parametrize(
    ("args", "origin"),
    [
        # A path with no dot in its first segment is the standard library.
        (("fmt",), "stdlib"),
        (("net/http",), "stdlib"),
        # Internal means "under the module path from go.mod".
        (("example.com/fixture/store", "example.com/fixture"), "internal"),
        (("example.com/fixture", "example.com/fixture"), "internal"),
        (("github.com/x/y", None, {"github.com/x/y"}), "third_party"),
        (("other.io/pkg",), "unknown"),
    ],
)
def test_classify_go_import(args, origin):
    assert classify_go_import(*args) == origin


@pytest.mark.parametrize(
    ("args", "origin"),
    [
        (("std::fs",), "stdlib"),
        (("core::mem",), "stdlib"),
        (("crate::visitor", "fixture"), "internal"),
        (("fixture::visitor", "fixture"), "internal"),
        (("serde::de", None, {"serde"}), "third_party"),
        (("mystery::thing",), "unknown"),
    ],
)
def test_classify_rust_import(args, origin):
    assert classify_rust_import(*args) == origin


@pytest.mark.parametrize(
    "call",
    [
        lambda: ImportClassifier(stdlib={"os"}).classify("os"),
        lambda: TsImportClassifier(stdlib={"node:fs"}).classify("node:fs"),
        lambda: classify_go_import("fmt"),
        lambda: classify_rust_import("std::fs"),
    ],
)
def test_every_classifier_answers_with_one_of_the_four_origins(call):
    assert call() in ORIGINS
