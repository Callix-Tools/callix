"""`SpanIndex` — position in a file → node in the graph.

Two tables, deliberately different: `enclosing()` searches full extents (used
to attribute a boundary or an occurrence to the declaration it sits in), `at()`
searches name spans (used to bind a resolver answer, which points at an
identifier, to the node that declares it). Both return the *innermost* match.
"""

from __future__ import annotations

from callix import Graph, Span, SpanIndex

from tests.graphs import node, spanned


def test_enclosing_returns_the_innermost_extent():
    index = SpanIndex()
    index.add_full("a.py", "outer", Span(1, 0, 10, 0))
    index.add_full("a.py", "inner", Span(2, 0, 4, 0))
    assert index.enclosing("a.py", 3, 0) == "inner"
    assert index.enclosing("a.py", 8, 0) == "outer"


def test_enclosing_is_insertion_order_independent():
    """The tightest span wins whichever order the entries arrived in."""
    outer_first, inner_first = SpanIndex(), SpanIndex()
    outer_first.add_full("a.py", "outer", Span(1, 0, 10, 0))
    outer_first.add_full("a.py", "inner", Span(2, 0, 4, 0))
    inner_first.add_full("a.py", "inner", Span(2, 0, 4, 0))
    inner_first.add_full("a.py", "outer", Span(1, 0, 10, 0))
    assert outer_first.enclosing("a.py", 3, 0) == inner_first.enclosing("a.py", 3, 0) == "inner"


def test_lookups_that_miss_return_none():
    index = SpanIndex()
    index.add_full("a.py", "outer", Span(1, 0, 10, 0))
    index.add_name("a.py", "outer", Span(1, 4, 1, 9))
    assert index.enclosing("b.py", 3, 0) is None      # unknown file
    assert index.enclosing("a.py", 99, 0) is None     # past every span
    assert index.at("a.py", 3, 0) is None             # no name span there
    assert SpanIndex().enclosing("a.py", 1, 1) is None  # empty index


def test_add_name_and_at_use_a_separate_table():
    index = SpanIndex()
    index.add_full("a.py", "fn", Span(1, 0, 5, 0))
    index.add_name("a.py", "fn", Span(1, 4, 1, 9))
    # A position inside the body is enclosed by the node but is not its name.
    assert index.enclosing("a.py", 3, 0) == "fn"
    assert index.at("a.py", 3, 0) is None
    # The name span is inclusive on both ends, like Span.contains.
    assert index.at("a.py", 1, 4) == "fn"
    assert index.at("a.py", 1, 9) == "fn"
    assert index.at("a.py", 1, 10) is None


def test_entries_added_after_the_first_lookup_are_still_found():
    """The table sorts lazily; a second add has to reset that."""
    index = SpanIndex()
    index.add_full("a.py", "first", Span(5, 0, 6, 0))
    assert index.enclosing("a.py", 5, 1) == "first"
    index.add_full("a.py", "second", Span(1, 0, 2, 0))
    assert index.enclosing("a.py", 1, 1) == "second"
    assert index.enclosing("a.py", 5, 1) == "first"


def test_from_graph_reads_span_and_metadata_name_span():
    graph = Graph()
    graph.add_node(spanned("outer", "a.py", Span(1, 0, 5, 0), Span(1, 4, 1, 6)))
    graph.add_node(spanned("inner", "a.py", Span(2, 0, 3, 0)))
    index = SpanIndex.from_graph(graph)
    assert index.enclosing("a.py", 2, 5) == "inner"
    assert index.enclosing("a.py", 4, 0) == "outer"
    # Only the node that carries name_span is in the name table.
    assert index.at("a.py", 1, 5) == "outer"
    assert index.at("a.py", 2, 1) is None


def test_from_graph_skips_nodes_without_a_file_or_a_span():
    graph = Graph()
    graph.add_node(node("no_file", span=Span(1, 0, 2, 0)))
    graph.add_node(node("no_span", file_path="a.py"))
    index = SpanIndex.from_graph(graph)
    assert index.enclosing("a.py", 1, 0) is None


def test_from_graph_ignores_a_malformed_name_span():
    """A hand-written graph must not break the index."""
    graph = Graph()
    graph.add_node(
        node("n", file_path="a.py", span=Span(1, 0, 2, 0), metadata={"name_span": "nonsense"})
    )
    index = SpanIndex.from_graph(graph)
    assert index.enclosing("a.py", 1, 0) == "n"
    assert index.at("a.py", 1, 0) is None


def test_from_graph_of_an_empty_graph():
    assert SpanIndex.from_graph(Graph()).enclosing("a.py", 1, 1) is None


def test_spans_are_per_file():
    index = SpanIndex()
    index.add_full("a.py", "a", Span(1, 0, 10, 0))
    index.add_full("b.py", "b", Span(1, 0, 10, 0))
    assert index.enclosing("a.py", 2, 0) == "a"
    assert index.enclosing("b.py", 2, 0) == "b"
