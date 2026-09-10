#!/usr/bin/env python3
"""Stamp major.minor.GITHUB_RUN_NUMBER in the package and its lockfile entry."""
import os
import re
import tomllib
from pathlib import Path


def stamp(root: Path, run_number: str) -> str:
    if not re.fullmatch(r"[1-9][0-9]*", run_number):
        raise ValueError("GITHUB_RUN_NUMBER must be a positive integer")
    manifest_path = root / "Cargo.toml"
    lock_path = root / "Cargo.lock"
    manifest = manifest_path.read_text()
    lock = lock_path.read_text()
    package = tomllib.loads(manifest)["package"]
    match = re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", package["version"])
    if not match:
        raise ValueError("The checked-in package version must be major.minor.patch")
    version = f"{match[1]}.{match[2]}.{run_number}"
    # Limit replacement to the package table; dependency versions remain unchanged.
    manifest, count = re.subn(
        r'(?ms)(^\[package\]\s*\n(?:(?!^\[).)*?^version\s*=\s*)"[^"\n]+"',
        lambda m: f'{m[1]}"{version}"',
        manifest,
    )
    if count != 1:
        raise ValueError("Expected exactly one package version in Cargo.toml")
    lock, count = re.subn(
        rf'(?m)(^name = "{re.escape(package["name"])}"\nversion = )"{re.escape(package["version"])}"',
        lambda m: f'{m[1]}"{version}"',
        lock,
    )
    if count != 1:
        raise ValueError("Expected exactly one matching package entry in Cargo.lock")
    # Validate both documents before writing either file.
    tomllib.loads(manifest)
    tomllib.loads(lock)
    manifest_path.write_text(manifest)
    lock_path.write_text(lock)
    return version


if __name__ == "__main__":
    root = Path(__file__).resolve().parents[2]
    version = stamp(root, os.environ["GITHUB_RUN_NUMBER"])
    print(f"CI crate version: {version}")
    if output := os.environ.get("GITHUB_OUTPUT"):
        with open(output, "a") as handle:
            handle.write(f"version={version}\n")
