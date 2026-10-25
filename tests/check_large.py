"""Exercise the nine-million-spike target with bounded RAM, including on Windows."""
import argparse
import ctypes
import json
import math
import os
from pathlib import Path
import random
import struct
import subprocess
import tempfile
import time

from check_features import require


def windows_peak(process):
    from ctypes import wintypes
    class Counters(ctypes.Structure):
        _fields_ = [("cb", wintypes.DWORD), ("faults", wintypes.DWORD)] + [
            (name, ctypes.c_size_t) for name in ("peak", "working", "peak_paged", "paged",
                                              "peak_nonpaged", "nonpaged", "pagefile", "peak_pagefile")]
    counters = Counters()
    counters.cb = ctypes.sizeof(counters)
    get_memory = ctypes.WinDLL("psapi").GetProcessMemoryInfo
    get_memory.argtypes = (wintypes.HANDLE, ctypes.POINTER(Counters), wintypes.DWORD)
    get_memory.restype = wintypes.BOOL
    if not get_memory(wintypes.HANDLE(int(process._handle)), ctypes.byref(counters), counters.cb):
        raise ctypes.WinError()
    return counters.peak


def check(exe, spikes, report, extra_args=()):
    require(spikes >= 250 and spikes % 250 == 0, "spikes must be a positive multiple of 250")
    rng = random.Random(943)
    bank = bytearray()
    for spike in range(250):
        for channel in range(4):
            bank += struct.pack("<I50b", spike, *(rng.randint(-100, 100) + channel for _ in range(50)))
    with tempfile.TemporaryDirectory(prefix="prekk scale ") as directory:
        root = Path(directory)
        recording = root / "scale.1"
        with recording.open("wb") as output:
            output.write(f"num_spikes {spikes}\r\ndata_start".encode())
            block = bank * 128
            repetitions, remainder = divmod(spikes // 250, 128)
            for _ in range(repetitions):
                output.write(block)
            output.write(bank * remainder)
        started = time.monotonic()
        process = subprocess.Popen([str(exe), str(recording), str(root / "large"), str(root / "count"), "3PC", *extra_args],
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        peak = 0
        while process.poll() is None:
            if os.name == "nt":
                try:
                    peak = max(peak, windows_peak(process))
                except OSError:
                    if process.poll() is None:
                        raise
                    break
            if time.monotonic() - started > 600:
                process.kill()
                process.communicate()
                raise RuntimeError("Large recording exceeded 10 minutes")
            time.sleep(0.05)
        stdout, stderr = process.communicate()
        seconds = time.monotonic() - started
        require(process.returncode == 0, stdout + stderr)
        if os.name != "nt":
            import resource
            # Maximum resident size of the Rust child on Unix.
            peak = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
            if os.sys.platform != "darwin":
                peak *= 1024
        require(peak > 0 and peak < 256 * 1024**2, f"Peak memory exceeds 256 MiB: {peak}")
        require(int((root / "count").read_text()) == spikes, "Wrong large-recording spike count")
        rows = 0
        first_period = []
        with (root / "large.fet.1").open() as features:
            require(int(next(features)) == 12, "Wrong large-recording feature width")
            for rows, line in enumerate(features, 1):
                if rows <= 250 or rows > spikes - 250:
                    values = list(map(float, line.split()))
                    require(len(values) == 12 and all(math.isfinite(v) for v in values), "Invalid large-recording features")
                    if rows <= 250:
                        first_period.append(values)
                    else:
                        require(values == first_period[(rows - 1) % 250], "Repeated waveforms produced inconsistent features")
        require(rows == spikes, f"Expected {spikes} rows, found {rows}")
    results = {"spikes": spikes, "seconds": round(seconds, 3), "peak_bytes": peak,
               "memory_measurement": "Windows process peak working set" if os.name == "nt" else "Unix maximum child RSS"}
    report.write_text(json.dumps(results, indent=2) + "\n")
    print(json.dumps(results))


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", type=Path, required=True)
    parser.add_argument("--backend", choices=("auto", "nalgebra", "lapack"), default="nalgebra")
    parser.add_argument("--lapack-library", type=Path)
    parser.add_argument("--spikes", type=int, default=9_000_000)
    parser.add_argument("--report", type=Path, default=Path("large-recording.json"))
    args = parser.parse_args()
    extra_args = ["--backend", args.backend]
    if args.lapack_library:
        extra_args += ["--lapack-library", str(args.lapack_library.resolve())]
    check(args.exe.resolve(), args.spikes, args.report.resolve(), extra_args)
