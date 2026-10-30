"""Verify the package's contents and exercise it outside the source tree."""
import argparse
import hashlib
import os
from pathlib import Path
import plistlib
import struct
import subprocess
import sys
import tempfile

from common import require
from check_benchmark import check_benchmark
from check_cli import check_cli, check_dumps
from check_cluster import check_cluster, check_queue
from check_concurrency import check_concurrency
from check_console import check_console
from check_features import check as check_features
from check_large import check as check_large
from check_shell import check_shell


def imports(exe):
    data = exe.read_bytes()
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    require(data[pe:pe+4] == b"PE\0\0", "Invalid PE header")
    machine, count = struct.unpack_from("<HH", data, pe + 4)
    require(machine == 0x8664, "Expected a Windows x64 executable")
    optional = pe + 24
    require(struct.unpack_from("<H", data, optional)[0] == 0x20B, "Expected PE32+")
    section_base = optional + struct.unpack_from("<H", data, pe + 20)[0]
    sections = [struct.unpack_from("<IIII", data, section_base + i * 40 + 8) for i in range(count)]
    def offset(rva):
        for virtual_size, address, raw_size, raw_start in sections:
            if address <= rva < address + max(virtual_size, raw_size):
                return raw_start + rva - address
        raise RuntimeError(f"Unmapped PE RVA: {rva}")
    directory = offset(struct.unpack_from("<I", data, optional + 120)[0])
    names = set()
    while any(data[directory:directory+20]):
        name = offset(struct.unpack_from("<I", data, directory + 12)[0])
        names.add(data[name:data.index(b"\0", name)].decode("ascii").lower())
        directory += 20
    return names


def check(package):
    executable = package / ("ltxk.exe" if os.name == "nt" else "ltxk")
    require((package / "assets/ltxk.svg").is_file(), "The README icon is missing from the package")
    for line in (package / "SHA256SUMS.txt").read_text().splitlines():
        expected, name = line.split("  ", 1)
        require(hashlib.sha256((package / name).read_bytes()).hexdigest() == expected, f"Checksum mismatch: {name}")
    if os.name == "nt":
        require({p.name for p in package.glob("*.dll")} == {"ltxk-shell.dll"}, "Unexpected runtime DLLs in the Windows package")
        for name in ("ltxk-launch.exe", "ltxk.msix", "ltxk.cer", "setup.ps1", "Install Explorer integration.cmd", "Uninstall Explorer integration.cmd"):
            require((package / name).is_file(), f"Missing Explorer integration: {name}")
        print("Windows startup DLLs:", ", ".join(sorted(imports(executable))))
        system = Path(os.environ["SystemRoot"])
        subprocess.run([str(system / "System32/WindowsPowerShell/v1.0/powershell.exe"),
                        "-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
                        str(Path(__file__).resolve().parent / "check_windows.ps1"),
                        "-PackageDirectory", str(package)], check=True)
        os.environ["PATH"] = os.pathsep.join(map(str, (system / "System32", system)))
    elif sys.platform == "darwin":
        app = package / "ltx-klusta.app"
        require((app / "Contents" / "Resources" / "ltxk").read_bytes() == executable.read_bytes(),
                "The app does not contain the packaged executable")
        with (app / "Contents/Info.plist").open("rb") as stream:
            metadata = plistlib.load(stream)
        require(metadata.get("CFBundleIconFile") == "ltxk.icns" and
                (app / "Contents/Resources/ltxk.icns").is_file(), "The app icon is missing")
        subprocess.run(["/usr/bin/codesign", "--verify", "--strict", "--deep", str(app)], check=True)
    with tempfile.TemporaryDirectory(prefix="ltxk isolated package ") as temporary:
        previous = Path.cwd()
        try:
            os.chdir(temporary)
            check_cli(executable)
            check_dumps(executable)
            check_features(executable)
            check_cluster(executable)
            check_queue(executable)
            check_console(executable)
            check_shell(executable)
            check_benchmark(executable)
            check_concurrency(executable)
            check_large(executable, 1_000_000, None, 256)
        finally:
            os.chdir(previous)
    print("The packaged executable passed every check")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--package", type=Path, required=True)
    check(parser.parse_args().package.resolve())
