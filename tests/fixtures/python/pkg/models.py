"""Types the service layer annotates against."""

import os
from typing import TypeAlias

Identifier: TypeAlias = int

DEFAULT_REGION = os.environ.get("REGION", "eu")


class Base:
    """Base with an attribute and a method."""

    label: str = "base"

    def describe(self) -> str:
        return self.label


class Engine(Base):
    def __init__(self, region: str) -> None:
        self.region = region

    def ping(self) -> bool:
        return True
