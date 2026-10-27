"""Package the executable, user documentation, checksums, and dependency notices."""
import argparse
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tomllib

from create_macos_app import create as create_macos_app


def package(exe, output, platform):
    output.mkdir(parents=True)
    shutil.copy2(exe, output / ("ltxk.exe" if platform == "windows" else "ltxk"))
    for name in ("README.md", "CLUSTERING.md", "ltxk.example.json"):
        shutil.copy2(name, output / name)
    if platform == "macos":
        create_macos_app(exe, output / "ltx-klusta.app")
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    compiler = subprocess.check_output(["rustc", "-vV"], text=True)
    (output / "BUILD.txt").write_text(
        f"Source commit: {commit}\n\n{compiler}", encoding="utf-8")
    cargo_home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    registry = cargo_home / "registry" / "src"
    notices = []
    for dependency in tomllib.loads(Path("Cargo.lock").read_text())["package"]:
        if not dependency.get("source", "").startswith("registry+"):
            continue
        name = f"{dependency['name']}-{dependency['version']}"
        for source in registry.glob(f"*/{name}"):
            for notice in sorted(source.iterdir()):
                if notice.is_file() and notice.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "NOTICE")):
                    notices.append(f"\n{'=' * 72}\n{name}/{notice.name}\n{'=' * 72}\n"
                                   + notice.read_text(encoding="utf-8", errors="replace"))
    if not notices:
        raise RuntimeError("Dependency licence notices were not found in the Cargo registry")
    (output / "THIRD_PARTY_NOTICES.txt").write_text("".join(notices), encoding="utf-8")
    checksums = [f"{hashlib.sha256(item.read_bytes()).hexdigest()}  {item.name}\n"
                 for item in sorted(output.iterdir()) if item.is_file()]
    (output / "SHA256SUMS.txt").write_text("".join(checksums), encoding="utf-8")
    archive = output.with_suffix(".zip")
    if platform == "macos":
        # ditto keeps the app bundle's permissions and code signature intact.
        subprocess.run(["/usr/bin/ditto", "-c", "-k", "--keepParent", str(output), str(archive)], check=True)
    else:
        shutil.make_archive(str(output), "zip", output.parent, output.name)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    archive.with_suffix(".zip.sha256").write_text(f"{digest}  {archive.name}\n")
    print(f"Packaged the {platform} build in {output} and {archive}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--platform", choices=("windows", "macos"), required=True)
    args = parser.parse_args()
    package(args.exe, args.output, args.platform)
