#!/usr/bin/env python3
"""Verify and bump synchronized FreeApiBox release versions.

Usage:
  python3 .github/scripts/release_version.py verify
  python3 .github/scripts/release_version.py bump 0.8.4
  python3 .github/scripts/release_version.py publish-update 0.8.4

The bump command changes the five manifests/lockfiles in one working tree.
Committing and pushing happens only after a successful GitHub Release.
"""
import json
import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PACKAGE = ROOT / "package.json"
PACKAGE_LOCK = ROOT / "package-lock.json"
TAURI = ROOT / "src-tauri" / "tauri.conf.json"
CARGO = ROOT / "src-tauri" / "Cargo.toml"
CARGO_LOCK = ROOT / "src-tauri" / "Cargo.lock"
UPDATE_FILE = ROOT / "update.txt"
CARGO_PACKAGE = "router-desktop"
VERSION_RE = re.compile(r"^\d+\.\d+\.\d+$")


def cargo_lock_version(contents: str) -> str:
    pattern = re.compile(
        r'(?m)^\[\[package\]\]\nname = "' + re.escape(CARGO_PACKAGE)
        + r'"\nversion = "([^"]+)"'
    )
    matches = pattern.findall(contents)
    if len(matches) != 1:
        raise ValueError("Cargo.lock must contain one router-desktop package version")
    return matches[0]


def verified_version() -> str:
    package = json.loads(PACKAGE.read_text(encoding="utf-8"))
    package_lock = json.loads(PACKAGE_LOCK.read_text(encoding="utf-8"))
    tauri = json.loads(TAURI.read_text(encoding="utf-8"))
    cargo = tomllib.loads(CARGO.read_text(encoding="utf-8"))
    versions = {
        "package.json": package["version"],
        "package-lock.json": package_lock["version"],
        "package-lock.json root package": package_lock["packages"][""]["version"],
        "tauri.conf.json": tauri["version"],
        "Cargo.toml": cargo["package"]["version"],
        "Cargo.lock": cargo_lock_version(CARGO_LOCK.read_text(encoding="utf-8")),
    }
    if len(set(versions.values())) != 1:
        raise ValueError(f"Release version mismatch: {versions}")
    current = next(iter(versions.values()))
    if not VERSION_RE.fullmatch(current):
        raise ValueError(f"Unsupported release version: {current!r}")
    return current


def replace_one(contents: str, before: str, after: str, path: str) -> str:
    if contents.count(before) != 1:
        raise ValueError(f"{path}: expected exactly one matching package version")
    return contents.replace(before, after, 1)


def write_json(path: Path, data: dict) -> None:
    path.write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def publish_update(version: str) -> str:
    """Record a successfully published version, never the next development version."""
    if not VERSION_RE.fullmatch(version):
        raise ValueError(f"Unsupported release version: {version!r}")
    previous = UPDATE_FILE.read_text(encoding="utf-8").strip() if UPDATE_FILE.exists() else ""
    if VERSION_RE.fullmatch(previous) and tuple(map(int, previous.split("."))) > tuple(map(int, version.split("."))):
        return previous
    UPDATE_FILE.write_text(version + "\n", encoding="utf-8")
    return version


def bump(expected: str) -> str:
    current = verified_version()
    if expected != current:
        raise ValueError(f"Expected released version {expected}, repository has {current}")
    major, minor, patch = map(int, current.split("."))
    following = f"{major}.{minor}.{patch + 1}"

    cargo = CARGO.read_text(encoding="utf-8")
    cargo_lock = CARGO_LOCK.read_text(encoding="utf-8")
    next_cargo = replace_one(cargo, f'version = "{current}"', f'version = "{following}"', "Cargo.toml")
    next_cargo_lock = replace_one(
        cargo_lock,
        f'name = "{CARGO_PACKAGE}"\nversion = "{current}"',
        f'name = "{CARGO_PACKAGE}"\nversion = "{following}"',
        "Cargo.lock",
    )

    package = json.loads(PACKAGE.read_text(encoding="utf-8"))
    package_lock = json.loads(PACKAGE_LOCK.read_text(encoding="utf-8"))
    tauri = json.loads(TAURI.read_text(encoding="utf-8"))
    package["version"] = following
    package_lock["version"] = following
    package_lock["packages"][""]["version"] = following
    tauri["version"] = following

    write_json(PACKAGE, package)
    write_json(PACKAGE_LOCK, package_lock)
    write_json(TAURI, tauri)
    CARGO.write_text(next_cargo, encoding="utf-8")
    CARGO_LOCK.write_text(next_cargo_lock, encoding="utf-8")
    if verified_version() != following:
        raise ValueError("Version bump failed post-write verification")
    return following


def main() -> int:
    try:
        if len(sys.argv) == 2 and sys.argv[1] == "verify":
            print(verified_version())
        elif len(sys.argv) == 3 and sys.argv[1] == "bump":
            print(bump(sys.argv[2]))
        elif len(sys.argv) == 3 and sys.argv[1] == "publish-update":
            print(publish_update(sys.argv[2]))
        else:
            print("Usage: release_version.py verify | bump <released-version> | publish-update <released-version>", file=sys.stderr)
            return 2
    except (ValueError, KeyError, OSError, tomllib.TOMLDecodeError) as exc:
        print(f"::error::{exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
