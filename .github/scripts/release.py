#!/usr/bin/env python3
"""The version arithmetic and the lockfile check `release.yml` runs.

This crate is released under the distribution's scheme, `major.minor.(patch * 100 +
amendment)`: the third field carries both a patch and an amendment, so `1.21.0` is a
base, `1.21.1`-`1.21.99` are amendments to it, and `1.21.100` is the next upstream
patch. A release therefore bumps one of three slots, and the arithmetic lives here so
the workflow's two jobs cannot disagree about it.

    release.py next amendment          # 1.21.3 -> 1.21.4
    release.py next patch              # 1.21.3 -> 1.21.100
    release.py next minor              # 1.21.3 -> 1.22.0
    release.py next none               # 1.21.3 -> 1.21.3   (a re-release)
    release.py next amendment --write  # ...and write it into the manifest

    release.py check-lock before.lock after.lock

`--write` rewrites exactly the `version` line of `[package]` and leaves the rest of the
manifest — which here is mostly commentary — byte for byte. `--manifest PATH` reads and
writes a manifest other than `./Cargo.toml`, which is what makes this testable without
touching the real one.

`check-lock` is the other half of a bump. The root package's version *is* recorded in
`Cargo.lock`, so bumping it moves the lock by construction — and a `--locked` build
would then refuse. The question the check answers is whether anything else moved with
it: a dependency that floated to a new version between two releases is a change to look
at, not one for a release to make quietly.
"""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path

DEFAULT_MANIFEST = Path("Cargo.toml")
"""The manifest `next` reads and writes when `--manifest` is not given."""

ROOT_PACKAGE = "bite-gp-pass"
"""The only package a bump is allowed to move in the lockfile."""


def flag(argv: list[str], name: str) -> str | None:
    """The value of `--name VALUE`, or `None` when the flag is absent."""
    if name not in argv:
        return None
    index = argv.index(name) + 1
    if index == len(argv):
        raise SystemExit(f"{name} needs a value")
    return argv[index]


def read_version(manifest: Path) -> str:
    with manifest.open("rb") as handle:
        return tomllib.load(handle)["package"]["version"]


def parse(version: str) -> tuple[int, int, int]:
    match = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)", version)
    if match is None:
        raise SystemExit(f"not a major.minor.patch version: {version!r}")
    major, minor, third = (int(part) for part in match.groups())
    return major, minor, third


def next_version(bump: str, version: str) -> str:
    major, minor, third = parse(version)
    patch, amendment = divmod(third, 100)

    if bump == "none":
        return version
    if bump == "amendment":
        if amendment == 99:
            raise SystemExit(
                f"{version} is the last amendment of its line. The slot above it, "
                f"{major}.{minor}.{(patch + 1) * 100}, is reserved for the next upstream "
                "patch and has to be taken by a `patch` bump, not invented by an "
                "`amendment` one."
            )
        return f"{major}.{minor}.{third + 1}"
    if bump == "patch":
        return f"{major}.{minor}.{(patch + 1) * 100}"
    if bump == "minor":
        return f"{major}.{minor + 1}.0"
    raise SystemExit(f"unknown bump: {bump!r} (want amendment, patch, minor or none)")


def write_version(manifest: Path, version: str) -> None:
    text = manifest.read_text()
    # From `[package]` onwards, and anchored to the line: a dependency's version is a
    # table key inside `[dependencies]`, never a bare `version = "..."` at column zero.
    start = text.index("[package]")
    match = re.compile(r'^version = "[^"]*"', re.MULTILINE).search(text, start)
    if match is None:
        raise SystemExit(f"no version line in [package] of {manifest}")
    manifest.write_text(
        text[: match.start()] + f'version = "{version}"' + text[match.end() :]
    )


def lock_entries(lock: Path) -> set[tuple[str, str]]:
    with lock.open("rb") as handle:
        data = tomllib.load(handle)
    return {(package["name"], package["version"]) for package in data.get("package", [])}


def check_lock(before: Path, after: Path) -> None:
    old, new = lock_entries(before), lock_entries(after)
    if old == new:
        raise SystemExit(
            "the lock did not take the bumped version: the root package's version is "
            "recorded in Cargo.lock, so a bump that leaves it unchanged did not happen"
        )
    moved = sorted(entry for entry in old ^ new if entry[0] != ROOT_PACKAGE)
    if moved:
        listed = ", ".join(f"{name} {version}" for name, version in moved)
        raise SystemExit(f"the lock moved beyond the version bump: {listed}")


def main(argv: list[str]) -> int:
    command = argv[1] if len(argv) > 1 else ""

    if command == "next":
        if len(argv) < 3:
            raise SystemExit("next needs a bump: amendment, patch, minor or none")
        bump = argv[2]
        manifest = Path(flag(argv, "--manifest") or DEFAULT_MANIFEST)
        version = next_version(bump, read_version(manifest))
        if "--write" in argv:
            write_version(manifest, version)
        print(version)
        return 0

    if command == "check-lock":
        if len(argv) < 4:
            raise SystemExit("check-lock needs the two lockfiles")
        check_lock(Path(argv[2]), Path(argv[3]))
        print(f"the lock moved only with the bump of {ROOT_PACKAGE}")
        return 0

    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
