#!/usr/bin/env python3
"""Install the packages of a composer.lock from git, without dist archives.

Some networks refuse GitHub's dist zipballs, and composer then falls back to
full clones that fill the cache with gigabytes. This installs every package
of `ROOT/composer.lock` from its `source.url` at `source.reference` with a
shallow fetch plus `git archive`, which honours `export-ignore` and so matches
the dist. It then writes `vendor/composer/installed.json`, so that a following
`composer install` finds nothing to install and only writes the autoloader.

    composer_git.py ROOT [--jobs N]
"""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


def fetch(package: dict, vendor: Path) -> str | None:
    """Install one package into vendor/<name>; return an error text or None."""
    name = package["name"]
    dest = vendor / name
    if (dest / "composer.json").exists():
        return None
    shutil.rmtree(dest, ignore_errors=True)
    dest.mkdir(parents=True)
    source = package["source"]
    with tempfile.TemporaryDirectory(dir=vendor) as tmp:
        for cmd in (
            ["git", "init", "-q", "--bare", tmp],
            ["git", "-C", tmp, "fetch", "-q", "--depth", "1", source["url"], source["reference"]],
        ):
            result = subprocess.run(cmd, capture_output=True, text=True, check=False)
            if result.returncode:
                return f"{name}: {result.stderr.strip()[-300:]}"
        archive = subprocess.Popen(
            ["git", "-C", tmp, "archive", "FETCH_HEAD"], stdout=subprocess.PIPE
        )
        untar = subprocess.run(
            ["tar", "-x", "-C", str(dest)], stdin=archive.stdout, check=False
        )
        archive.wait()
        if archive.returncode or untar.returncode:
            return f"{name}: git archive failed"
    return None


def install(root: Path, jobs: int = 2) -> list[str]:
    """Install every package of ROOT/composer.lock; return the list of errors."""
    lock = json.loads((root / "composer.lock").read_text(encoding="utf-8"))
    packages = lock["packages"] + lock["packages-dev"]
    errors = [f"{p['name']}: no git source" for p in packages if not p.get("source")]
    if errors:
        return errors
    vendor = root / "vendor"
    vendor.mkdir(exist_ok=True)
    with concurrent.futures.ThreadPoolExecutor(jobs) as pool:
        errors = [e for e in pool.map(lambda p: fetch(p, vendor), packages) if e]
    if errors:
        return errors
    installed = {
        "packages": [dict(p, **{"install-path": "../" + p["name"]}) for p in packages],
        "dev": True,
        "dev-package-names": sorted(p["name"] for p in lock["packages-dev"]),
    }
    (vendor / "composer").mkdir(exist_ok=True)
    (vendor / "composer" / "installed.json").write_text(
        json.dumps(installed, indent=4), encoding="utf-8"
    )
    return []


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("root", type=Path)
    parser.add_argument("--jobs", type=int, default=2)
    args = parser.parse_args()
    errors = install(args.root, args.jobs)
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print("installed all packages from git")
    return 0


if __name__ == "__main__":
    sys.exit(main())
