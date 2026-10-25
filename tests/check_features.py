"""Exercise signed waveforms, PCA, amplitude/VT, and malformed large inputs."""

import argparse
import math
from pathlib import Path
import random
import struct
import subprocess
import tempfile


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def run(exe, tetrode, root, components, ok=True, extra_args=()):
    base = root / "features with spaces"
    info = root / "spike count.txt"
    result = subprocess.run(
        [str(exe), "features", str(tetrode), str(base), str(info), components, *extra_args],
        capture_output=True, text=True, timeout=90,
    )
    require((result.returncode == 0) == ok, result.stdout + result.stderr)
    if not ok:
        return result.stderr
    lines = base.with_suffix(".fet.1").read_text().splitlines()
    columns = int(lines[0])
    rows = [list(map(float, line.split())) for line in lines[1:]]
    require(all(len(row) == columns for row in rows), "Wrong feature width")
    require(int(info.read_text()) == len(rows), "Incorrect spike count")
    require(all(math.isfinite(x) for row in rows for x in row), "Non-finite feature")
    return rows


def check(exe, extra_args=()):
    with tempfile.TemporaryDirectory(prefix="fancykk test ") as directory:
        root = Path(directory)
        tetrode = root / "input with spaces.1"
        rng = random.Random(943)
        # Full-rank, non-degenerate signed data with different channel offsets.
        waves = [[[rng.randint(-100, 100) + channel for _ in range(50)]
                  for channel in range(4)] for _ in range(80)]
        for active in (4, 2):
            payload = bytearray()
            for spike, channels in enumerate(waves):
                for channel, wave in enumerate(channels):
                    payload += struct.pack("<I", spike * 10)
                    payload += struct.pack("50b", *(wave if channel < active else [0] * 50))
            tetrode.write_bytes(b"num_spikes 80\r\ndata_start" + payload + b"\r\ndata_end\r\n")
            for components in ("A", "VT", "3PC", "3PC,A,VT"):
                rows = run(exe, tetrode, root, components, extra_args=extra_args)
                require(len(rows) == len(waves), "Missing feature rows")
                expected_width = active * (5 if components == "3PC,A,VT" else
                                           3 if components == "3PC" else 1)
                require(len(rows[0]) == expected_width, "Wrong active channel count")
                if components in ("A", "VT"):
                    for channel in range(active):
                        variances = []
                        for sample in range(50):
                            values = [spike[channel][sample] for spike in waves]
                            mean = sum(values) / len(values)
                            variances.append(sum((x - mean) ** 2 for x in values))
                        at = max(range(50), key=lambda i: variances[i])
                        for spike, row in zip(waves, rows):
                            expected = (max(spike[channel]) - min(spike[channel])
                                        if components == "A" else spike[channel][at])
                            require(row[channel] == expected, "Amplitude/VT mismatch")
                if components == "3PC":
                    for channel in range(active):
                        columns = [[row[channel * 3 + j] for row in rows] for j in range(3)]
                        energies = [sum(x * x for x in col) for col in columns]
                        require(energies == sorted(energies, reverse=True), "PCA order")
                        for i, column in enumerate(columns):
                            require(abs(sum(column) / len(column)) < 1e-3, "PCA centering")
                            for previous in columns[:i]:
                                dot = sum(a * b for a, b in zip(column, previous))
                                scale = math.sqrt(sum(x*x for x in column) * sum(x*x for x in previous))
                                require(abs(dot) / scale < 1e-5, "PCA orthogonality")
        for count, message in ((-1, "at least 2"), (0, "at least 2"),
                               (9000000, "shorter"), (10000000, "shorter"),
                               (2147483648, "2147483647")):
            tetrode.write_bytes(f"num_spikes {count}\r\ndata_start".encode())
            require(message in run(exe, tetrode, root, "3PC", ok=False, extra_args=extra_args), "Wrong count error")
        for content in (b"num_spikes nonsense\ndata_start", b"num_spikes 80\n",
                        b"data_start", b"not a tetrode file"):
            tetrode.write_bytes(content)
            run(exe, tetrode, root, "3PC", ok=False, extra_args=extra_args)
    print("Feature mathematics and input checks passed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", required=True, type=Path)
    parser.add_argument("--backend", choices=("auto", "nalgebra", "lapack"))
    parser.add_argument("--lapack-library", type=Path)
    parser.add_argument("--block-spikes", type=int)
    args = parser.parse_args()
    extra_args = []
    if args.backend:
        extra_args += ["--backend", args.backend]
    if args.block_spikes:
        extra_args += ["--block-spikes", str(args.block_spikes)]
    if args.lapack_library:
        extra_args += ["--lapack-library", str(args.lapack_library.resolve())]
    check(args.exe.resolve(), extra_args)
