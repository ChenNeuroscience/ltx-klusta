"""Cluster a large recording end to end with bounded memory (default: one million spikes)."""
import argparse
import ctypes
import json
import os
from pathlib import Path
import random
import struct
import subprocess
import tempfile
import time

from common import require


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


def write_recording(path, spikes):
    """Three units on four channels, repeating a 250-spike bank of noisy waveforms."""
    rng = random.Random(943)
    bank = bytearray()
    for spike in range(250):
        unit = spike % 3
        for channel in range(4):
            peak = (40, -30, 70)[unit] * (1 + 0.2 * channel)
            bank += struct.pack("<I50b", spike, *(max(-127, min(127, round(
                peak * (1 if 10 <= j < 20 else 0.2) + rng.gauss(0, 5)))) for j in range(50)))
    with path.open("wb") as output:
        output.write(f"num_spikes {spikes}\r\ndata_start".encode())
        repetitions, remainder = divmod(spikes // 250, 128)
        for _ in range(repetitions):
            output.write(bank * 128)
        output.write(bank * remainder)


def check(exe, spikes, report, limit_mib):
    require(spikes >= 250 and spikes % 250 == 0, "spikes must be a positive multiple of 250")
    with tempfile.TemporaryDirectory(prefix="ltxk large ") as directory:
        root = Path(directory)
        recording = root / "large.1"
        write_recording(recording, spikes)
        started = time.monotonic()
        process = subprocess.Popen([str(exe), "cluster", str(recording), "--no-queue", "--jobs", "1"],
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
            if time.monotonic() - started > 1800:
                process.kill()
                process.communicate()
                raise RuntimeError("Large recording exceeded 30 minutes")
            time.sleep(0.05)
        stdout, stderr = process.communicate()
        seconds = time.monotonic() - started
        require(process.returncode == 0, stdout + stderr)
        if os.name != "nt":
            import resource
            # Maximum resident size of the child; bytes on macOS, KiB on Linux.
            peak = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
            if os.sys.platform != "darwin":
                peak *= 1024
        require(0 < peak < limit_mib * 1024**2, f"Peak memory exceeds {limit_mib} MiB: {peak}")
        with recording.with_suffix(".clu.1").open() as labels:
            total = int(next(labels))
            rows = sum(1 for _ in labels)
        require(rows == spikes, f"Expected {spikes} labels, found {rows}")
        require(total >= 4, f"Expected at least three signal clusters plus noise, found {total}")
    results = {"spikes": spikes, "seconds": round(seconds, 3), "peak_bytes": peak,
               "memory_measurement": "Windows process peak working set" if os.name == "nt" else "Unix maximum child RSS"}
    if report:
        report.write_text(json.dumps(results, indent=2) + "\n")
    print(json.dumps(results))


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", type=Path, required=True)
    parser.add_argument("--spikes", type=int, default=1_000_000)
    parser.add_argument("--memory-limit-mib", type=int, default=256)
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    check(args.exe.resolve(), args.spikes, args.report and args.report.resolve(), args.memory_limit_mib)
