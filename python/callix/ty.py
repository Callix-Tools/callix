"""A symbol resolver built on ty, linked into the module itself.

No external `ty` binary or `ty server` subprocess is needed: the type checker
and the typeshed stubs are compiled into the native module. Implements the
protocol
:class:`callix.SymbolResolver`.
"""

from __future__ import annotations

import sys
from pathlib import Path

from ._core import EmbeddedTyResolver as _Native
from ._core import ResolvedRef, ResolverStatus


class TyResolver:
    """Resolves symbols through the embedded ty."""

    def __init__(self, base_prefix: str | Path | None = None) -> None:
        """
        Args:
            base_prefix: the Python installation root used to recognize
                stdlib outside typeshed. Defaults to ``sys.base_prefix``.

        """
        self._inner = _Native(str(base_prefix or sys.base_prefix))

    def prepare(self, project_root: Path, files: list[Path]) -> None:
        """Brings up ty's database for the project."""
        self._inner.prepare(str(project_root), [str(f) for f in files])

    def resolve_all(
        self, queries: list[tuple[Path, int, int]]
    ) -> list[ResolvedRef | None]:
        """A batch of positions → definitions, order preserved."""
        return self._inner.resolve_all(
            [(str(path), line, col) for path, line, col in queries]
        )

    def definition_at(self, file: Path, line: int, col: int) -> ResolvedRef | None:
        """One position → a definition."""
        return self._inner.definition_at(str(file), line, col)

    def status(self) -> ResolverStatus:
        return self._inner.status()

    def __repr__(self) -> str:
        return repr(self._inner)
