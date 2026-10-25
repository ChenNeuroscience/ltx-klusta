"""Verify shipped dependencies and exercise a package outside the source tree."""
import argparse
import hashlib
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile

from check_features import check as check_features, require
from check_rust import check as check_rust
from check_cluster import check_cluster, check_queue


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


def runtime_selection(executable, native_flags):
    from check_rust import command, write_recording
    with tempfile.TemporaryDirectory(prefix="ltxk backend ") as temporary:
        root = Path(temporary)
        recording = root / "wave.1"
        write_recording(recording, [[(i-10)*((j%5)-2) for j in range(50)] for i in range(21)])
        args = ("features", recording, root / "out", root / "count", "3PC")
        default = command(executable, *args)
        require(default.returncode == 0 and "nalgebra backend" in default.stderr, default.stderr)
        automatic = command(executable, *args, "--backend", "auto")
        expected = "nalgebra backend" if os.name == "nt" else "lapack ("
        require(automatic.returncode == 0 and expected in automatic.stderr, automatic.stderr)
        explicit = command(executable, *args, *native_flags)
        require(explicit.returncode == 0 and "lapack (" in explicit.stderr, explicit.stderr)
        for backend in ("auto", "lapack"):
            invalid = command(executable, *args, "--backend", backend, "--lapack-library", root / "missing-library")
            require(invalid.returncode != 0, "An explicit missing provider must be reported")
        provider = native_flags[-1] if "--lapack-library" in native_flags else None
        if provider:
            environment = os.environ.copy()
            environment["LTXK_LAPACK_LIBRARY"] = str(provider)
            automatic = command(executable, *args, "--backend", "auto", env=environment)
            require(automatic.returncode == 0 and "lapack (" in automatic.stderr, automatic.stderr)


def check(package, library):
    executable = package / ("ltxk.exe" if os.name == "nt" else "ltxk")
    for line in (package / "SHA256SUMS.txt").read_text().splitlines():
        expected, name = line.split("  ", 1)
        require(hashlib.sha256((package / name).read_bytes()).hexdigest() == expected, f"Checksum mismatch: {name}")
    require(not list(package.glob("*.dll")), "The package should contain no numerical DLLs")
    if os.name == "nt":
        actual = {name for name in imports(executable) if "blas" in name or "lapack" in name}
        require(not actual, f"Numerical DLLs must not be startup dependencies: {actual}")
        print("Windows startup DLLs:", ", ".join(sorted(imports(executable))))
        system = Path(os.environ["SystemRoot"])
        os.environ["PATH"] = os.pathsep.join(map(str, (system / "System32", system)))
    elif sys.platform == "darwin":
        dependencies = subprocess.check_output(["/usr/bin/otool", "-L", str(executable)], text=True)
        require("Accelerate.framework" not in dependencies, "Accelerate must be loaded only at runtime")
    os.environ.pop("LTXK_LAPACK_LIBRARY", None)
    native_flags = ["--backend", "lapack"]
    if library:
        native_flags += ["--lapack-library", str(library)]
    with tempfile.TemporaryDirectory(prefix="ltxk isolated package ") as temporary:
        previous = Path.cwd()
        try:
            os.chdir(temporary)
            check_features(executable, ("--backend", "nalgebra", "--block-spikes", "7"))
            check_rust(executable, ("--backend", "nalgebra"))
            check_cluster(executable, ("--backend", "nalgebra"))
            check_queue(executable)
            runtime_selection(executable, native_flags)
            check_features(executable, (*native_flags, "--block-spikes", "7"))
            check_rust(executable, native_flags)
            check_cluster(executable, native_flags)
        finally:
            os.chdir(previous)
    print("The same executable passed nalgebra, auto detection, and runtime LAPACK checks")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--package", type=Path, required=True)
    parser.add_argument("--lapack-library", type=Path, default=os.environ.get("LTXK_TEST_LAPACK_LIBRARY"))
    args = parser.parse_args()
    check(args.package.resolve(), Path(args.lapack_library).resolve() if args.lapack_library else None)
