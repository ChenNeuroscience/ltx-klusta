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


def check(package, backend, baseline):
    executable = package / ("prekk.exe" if os.name == "nt" else "prekk")
    for line in (package / "SHA256SUMS.txt").read_text().splitlines():
        expected, name = line.split("  ", 1)
        require(hashlib.sha256((package / name).read_bytes()).hexdigest() == expected, f"Checksum mismatch: {name}")
    if os.name == "nt":
        required = {"blas_win64_mt.dll", "lapack_win64_mt.dll"} if backend == "lapack" else set()
        actual = {name for name in imports(executable) if "blas" in name or "lapack" in name}
        require(actual == required, f"Unexpected numerical DLL imports: {actual}")
        require({p.name.lower() for p in package.glob('*.dll')} == required, "Wrong packaged DLLs")
        print("Windows DLL imports:", ", ".join(sorted(imports(executable))))
        system = Path(os.environ["SystemRoot"])
        # Child processes can see only their own directory and Windows system DLLs.
        os.environ["PATH"] = os.pathsep.join(map(str, (system / "System32", system)))
    elif sys.platform == "darwin":
        dependencies = subprocess.check_output(["/usr/bin/otool", "-L", str(executable)], text=True)
        require(("Accelerate.framework" in dependencies) == (backend == "lapack"), "Unexpected Accelerate linkage")
    with tempfile.TemporaryDirectory(prefix="prekk isolated package ") as directory:
        previous = Path.cwd()
        try:
            os.chdir(directory)
            check_features(executable, baseline, ("--block-spikes", "7"))
            if backend == "lapack":
                check_features(executable, baseline, ("--backend", "nalgebra", "--block-spikes", "7"))
            check_rust(executable, baseline)
        finally:
            os.chdir(previous)
    print(f"Standalone {backend} package passed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--package", type=Path, required=True)
    parser.add_argument("--backend", choices=("nalgebra", "lapack"), required=True)
    parser.add_argument("--baseline", type=Path, required=True)
    args = parser.parse_args()
    check(args.package.resolve(), args.backend, args.baseline.resolve())
