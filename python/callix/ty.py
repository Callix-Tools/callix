"""Резолвер символов на ty, слинкованном в сам модуль.

Внешний бинарь `ty` и подпроцесс `ty server` не нужны: тайп-чекер и стабы
typeshed вкомпилированы в нативный модуль. Реализует протокол
:class:`callix.SymbolResolver`.
"""

from __future__ import annotations

import sys
from pathlib import Path

from ._core import EmbeddedTyResolver as _Native
from ._core import ResolvedRef, ResolverStatus


class TyResolver:
    """Разрешает символы через встроенный ty."""

    def __init__(self, base_prefix: str | Path | None = None) -> None:
        """
        Args:
            base_prefix: корень установки Python, по которому опознаётся
                stdlib вне typeshed. По умолчанию ``sys.base_prefix``.

        """
        self._inner = _Native(str(base_prefix or sys.base_prefix))

    def prepare(self, project_root: Path, files: list[Path]) -> None:
        """Поднимает базу ty для проекта."""
        self._inner.prepare(str(project_root), [str(f) for f in files])

    def resolve_all(
        self, queries: list[tuple[Path, int, int]]
    ) -> list[ResolvedRef | None]:
        """Батч позиций → определения, порядок сохраняется."""
        return self._inner.resolve_all(
            [(str(path), line, col) for path, line, col in queries]
        )

    def definition_at(self, file: Path, line: int, col: int) -> ResolvedRef | None:
        """Одна позиция → определение."""
        return self._inner.definition_at(str(file), line, col)

    def status(self) -> ResolverStatus:
        return self._inner.status()

    def __repr__(self) -> str:
        return repr(self._inner)
