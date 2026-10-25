"""Fetch a pinned Windows LP64 OpenBLAS provider for CI only; never package it."""
import argparse
import hashlib
from pathlib import Path
import shutil
import urllib.request
import zipfile

URL = "https://github.com/OpenMathLib/OpenBLAS/releases/download/v0.3.34/OpenBLAS-0.3.34-x64.zip"
SHA256 = "e9cb6134541f36c27346d5fc5995652f060fba227cebbbabcbda5a5a44d7c76b"


def fetch(output, env_file):
    output.mkdir(parents=True)
    archive = output / "openblas.zip"
    urllib.request.urlretrieve(URL, archive)
    if hashlib.sha256(archive.read_bytes()).hexdigest() != SHA256:
        raise RuntimeError("OpenBLAS download checksum mismatch")
    with zipfile.ZipFile(archive) as bundle:
        bundle.extractall(output / "unpacked")
    runtime = output / "runtime"
    runtime.mkdir()
    for library in (output / "unpacked").rglob("*.dll"):
        shutil.copy2(library, runtime / library.name)
    providers = [p for p in runtime.glob("*.dll") if "openblas" in p.name.lower()]
    if len(providers) != 1:
        raise RuntimeError(f"Expected one OpenBLAS DLL, found {providers}")
    provider = providers[0].resolve()
    if env_file:
        with env_file.open("a", encoding="utf-8") as env:
            env.write(f"PREKK_TEST_LAPACK_LIBRARY={provider}\nPREKK_LAPACK_LIBRARY={provider}\n")
    print(f"CI-only LAPACK provider: {provider}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--env-file", type=Path)
    args = parser.parse_args()
    fetch(args.output, args.env_file)
