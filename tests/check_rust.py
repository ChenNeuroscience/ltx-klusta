"""Check the Rust CLI, chunk boundaries, degenerate PCA, and output safety."""
import argparse
import math
from pathlib import Path
import struct
import subprocess
import tempfile

from check_features import require, run


def command(exe, *args):
    return subprocess.run([str(exe), *map(str, args)], capture_output=True, text=True, timeout=90)


def write_recording(path, waves):
    payload = bytearray()
    for index, wave in enumerate(waves):
        payload += struct.pack("<I50b", index, *wave)
        payload += bytes(3 * 54)
    path.write_bytes(f"num_spikes {len(waves)}\r\ndata_start".encode() + payload)


def check(exe, baseline):
    for flag in ("--help", "-h", "--version", "-V"):
        result = command(exe, flag)
        require(result.returncode == 0 and result.stdout, f"CLI {flag} failed: {result.stderr}")
    for shell in ("bash", "zsh", "fish", "powershell", "elvish"):
        result = command(exe, "--completions", shell)
        require(result.returncode == 0 and "prekk" in result.stdout, result.stderr)
    require(command(exe).returncode == 2, "Missing arguments should be a CLI error")
    positional = ["x.1", "out", "count", "3PC"]
    for flags in (("--backend", "typo"), ("--block-spikes", "0"),
                  ("--block-spikes", "65537"), ("--block-spikes", "oops"), ("--blok-spikes", "7")):
        result = command(exe, *positional, *flags)
        require(result.returncode == 2 and "error:" in result.stderr, "CLI validation failed")
    with tempfile.TemporaryDirectory(prefix="prekk rust ") as directory:
        root = Path(directory)
        tetrode = root / "data.1"
        rank_one = [[(i - 20) * ((j % 5) - 2) + 4 for j in range(50)] for i in range(41)]
        constant_wave = [[j - 25 for j in range(50)] for _ in range(41)]
        # Repeated largest eigenvalues: compare retained subspaces via score Gram
        # matrices, because individual eigenvectors are not uniquely defined.
        tied = [[a, b] + [0] * 48 for a, b in [(-7, 0), (0, -7), (7, 0), (0, 7)] * 20]
        for waves, components in ((rank_one, "3PC,A,VT"), (constant_wave, "3PC,A,VT"), (tied, "2PC")):
            write_recording(tetrode, waves)
            original = tetrode.read_bytes()
            reference = run(baseline, tetrode, root, components) if baseline else None
            outputs = []
            for block in (1, 7, 4096):
                rows = run(exe, tetrode, root, components, extra_args=("--block-spikes", str(block)))
                outputs.append(rows)
                if reference:
                    pcs = int(components[0])
                    for i in range(len(rows)):
                        for j in range(len(rows)):
                            dot = sum(a * b for a, b in zip(rows[i][:pcs], rows[j][:pcs]))
                            expected = sum(a * b for a, b in zip(reference[i][:pcs], reference[j][:pcs]))
                            require(math.isclose(dot, expected, rel_tol=2e-5, abs_tol=1e-5), "PCA subspace differs")
                    require(all(a[pcs:] == b[pcs:] for a, b in zip(rows, reference)), "A/VT changed")
            require(outputs[0] == outputs[1] == outputs[2], "Chunk size changed output")
            result = command(exe, tetrode, root / "output", tetrode, components)
            require(result.returncode != 0 and tetrode.read_bytes() == original, "Input overwritten by count output")
            unicode_root = root / "unicode λ"
            unicode_root.mkdir(exist_ok=True)
            unicode_input = unicode_root / "data λ.1"
            unicode_input.write_bytes(original)
            unicode_rows = run(exe, unicode_input, unicode_root, components)
            require(unicode_rows == outputs[0], "Unicode paths changed extraction")
            alias = root / "alias.fet.1"
            alias.write_bytes(original)
            result = command(exe, alias, root / "alias", root / "count", components)
            require(result.returncode != 0 and alias.read_bytes() == original, "Input overwritten by features")
    print("Rust CLI, chunk boundaries, degenerate PCA, Unicode paths, and output safety passed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", required=True, type=Path)
    parser.add_argument("--baseline", type=Path)
    args = parser.parse_args()
    check(args.exe.resolve(), args.baseline.resolve() if args.baseline else None)
