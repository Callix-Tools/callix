"""Calls, references, annotations and a boundary."""

import httpx

from pkg.models import Base, Engine, Identifier


def build(region: str) -> Engine:
    engine = Engine(region)
    engine.ping()
    return engine


def inspect(item: Base) -> str:
    return item.describe()


def fetch_one(ident: Identifier) -> str:
    response = httpx.get(f"/engines/{ident}")
    return response.text
