#!/usr/bin/env python3
# SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
# SPDX-License-Identifier: Apache-2.0

"""Prepare an orig tarball and build tree, downloading locked crates once."""

import argparse
import io
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import tomllib


def git(*args):
    return subprocess.check_output(["git", *args], cwd=repo)


parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("output", type=Path, help="directory for the tarball and build tree")
args = parser.parse_args()
repo = Path(__file__).resolve().parent.parent
output = args.output.resolve()
output.mkdir(parents=True, exist_ok=True)

with tempfile.TemporaryDirectory(prefix=".corium-orig-", dir=output) as tmp:
    staging = Path(tmp)
    tree = staging / "source"
    tree.mkdir()
    # Export committed upstream files only; use the checkout's debian/ below.
    with tarfile.open(fileobj=io.BytesIO(git("archive", "HEAD"))) as archive:
        archive.extractall(tree, filter="data")
    shutil.rmtree(tree / "debian", ignore_errors=True)
    package = tomllib.loads((tree / "Cargo.toml").read_text())["package"]
    name = f"{package['name']}-{package['version']}"
    orig = f"{package['name']}_{package['version']}.orig.tar.xz"
    if (output / name).exists() or (output / orig).exists():
        parser.error("output already contains this version; use a fresh directory")
    config = subprocess.check_output(
        ["cargo", "vendor", "--locked", "--versioned-dirs", "vendor"], cwd=tree
    )
    # Match arma/Copr: do not append a second [net] if offline is already set.
    if not any(line.startswith(b"offline") for line in config.splitlines()):
        config = config + b"\n[net]\noffline = true\n"
    (tree / ".cargo").mkdir(exist_ok=True)
    (tree / ".cargo/config.toml").write_bytes(config)

    with (tree / "LICENSE.dependencies").open("w") as notes:
        notes.write("Bundled crates from Cargo.lock; original license choices retained.\n")
        for manifest in sorted((tree / "vendor").glob("*/Cargo.toml")):
            crate = tomllib.loads(manifest.read_text())["package"]
            notes.write(f"\n{'=' * 72}\n{crate['name']} {crate['version']}\n")
            notes.write(f"License: {crate.get('license', crate.get('license-file', 'unspecified'))}\n")
            notes.write(f"Authors: {', '.join(crate.get('authors', []))}\n")
            for path in sorted(manifest.parent.rglob("*")):
                if path.is_file() and path.name.lower().startswith(
                    ("license", "copying", "notice")
                ) and path.name != "licenserc.toml":
                    notes.write(f"\n--- {path.relative_to(manifest.parent)} ---\n")
                    notes.write(path.read_text())
                    notes.write("\n")

    tree.rename(staging / name)
    tree = staging / name
    # Normalize archive metadata for repeatable output from the same commit.
    timestamp = git("show", "-s", "--format=%ct", "HEAD").decode().strip()
    subprocess.run(
        ["tar", "--sort=name", f"--mtime=@{timestamp}", "--owner=0", "--group=0",
         "--numeric-owner", "--mode=u+rwX,go+rX,go-w", "-cJf", str(staging / orig),
         "-C", str(staging), name],
        check=True,
    )
    shutil.copytree(repo / "debian", tree / "debian")
    (staging / orig).rename(output / orig)
    tree.rename(output / name)

print(output / orig)
print(output / name)
