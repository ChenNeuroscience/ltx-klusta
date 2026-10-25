"""Check the CLI, chunk boundaries, analytic PCA cases, and output safety."""
import argparse
import math
from pathlib import Path
import struct
import subprocess
import tempfile

from check_features import require, run


def command(exe, *args, env=None):
    return subprocess.run([str(exe), *map(str, args)], capture_output=True, text=True,
                          encoding="utf-8", timeout=90, env=env)


def write_recording(path, waves):
    payload = bytearray()
    for index, wave in enumerate(waves):
        payload += struct.pack("<I50b", index, *wave)
        payload += bytes(3 * 54)
    path.write_bytes(f"num_spikes {len(waves)}\r\ndata_start".encode() + payload)


def check(exe, extra_args=()):
    for flag in ("--help", "-h", "--version", "-V"):
        result = command(exe, flag)
        require(result.returncode == 0 and result.stdout, f"CLI {flag} failed: {result.stderr}")
    for shell in ("bash", "zsh", "fish", "powershell", "elvish"):
        result = command(exe, "--completions", shell)
        require(result.returncode == 0 and "fancykk" in result.stdout, result.stderr)
    require(command(exe).returncode == 2, "Missing arguments should be a CLI error")
    positional = ["features", "x.1", "out", "count", "3PC"]
    for flags in (("--backend", "typo"), ("--block-spikes", "0"),
                  ("--block-spikes", "65537"), ("--block-spikes", "oops"), ("--blok-spikes", "7"),
                  ("--backend", "nalgebra", "--lapack-library", "x.dll")):
        result = command(exe, *positional, *flags)
        require(result.returncode == 2 and "error:" in result.stderr, "CLI validation failed")
    with tempfile.TemporaryDirectory(prefix="fancykk rust ") as directory:
        root = Path(directory)
        tetrode = root / "data.1"
        rank_one = [[(i - 20) * ((j % 5) - 2) + 4 for j in range(50)] for i in range(41)]
        constant_wave = [[j - 25 for j in range(50)] for _ in range(41)]
        tied = [[a, b] + [0] * 48 for a, b in [(-7, 0), (0, -7), (7, 0), (0, 7)] * 20]
        cases = ((rank_one, "3PC,A,VT", lambda i, j: 100 * (i - 20) * (j - 20)),
                 (constant_wave, "3PC,A,VT", lambda i, j: 0),
                 (tied, "2PC", lambda i, j: sum(a*b for a, b in zip(tied[i], tied[j]))))
        for waves, components, expected_dot in cases:
            write_recording(tetrode, waves)
            original = tetrode.read_bytes()
            outputs = []
            for block in (1, 7, 4096):
                rows = run(exe, tetrode, root, components, extra_args=(*extra_args, "--block-spikes", str(block)))
                outputs.append(rows)
                pcs = int(components[0])
                for i in range(len(rows)):
                    for j in range(len(rows)):
                        dot = sum(a*b for a, b in zip(rows[i][:pcs], rows[j][:pcs]))
                        require(math.isclose(dot, expected_dot(i, j), rel_tol=2e-5, abs_tol=1e-5),
                                "PCA does not preserve the known signal subspace")
                if components.endswith("A,VT"):
                    variances = []
                    for sample in range(50):
                        values = [wave[sample] for wave in waves]
                        mean = sum(values) / len(values)
                        variances.append(sum((value-mean)**2 for value in values))
                    at = max(range(50), key=lambda sample: variances[sample])
                    require(all(row[pcs:] == [max(wave)-min(wave), wave[at]]
                                for wave, row in zip(waves, rows)), "A/VT values are incorrect")
            require(outputs[0] == outputs[1] == outputs[2], "Chunk size changed output")
            result = command(exe, "features", tetrode, root / "output", tetrode, components, *extra_args)
            require(result.returncode != 0 and tetrode.read_bytes() == original, "Input overwritten by count output")
            unicode_root = root / "unicode λ"
            unicode_root.mkdir(exist_ok=True)
            unicode_input = unicode_root / "data λ.1"
            unicode_input.write_bytes(original)
            require(run(exe, unicode_input, unicode_root, components, extra_args=extra_args) == outputs[0],
                    "Unicode paths changed extraction")
            alias = root / "alias.fet.1"
            alias.write_bytes(original)
            result = command(exe, "features", alias, root / "alias", root / "count", components, *extra_args)
            require(result.returncode != 0 and alias.read_bytes() == original, "Input overwritten by features")
    print("CLI, chunk boundaries, analytic PCA, Unicode paths, and output safety passed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", required=True, type=Path)
    parser.add_argument("--backend", choices=("auto", "nalgebra", "lapack"), default="nalgebra")
    parser.add_argument("--lapack-library", type=Path)
    args = parser.parse_args()
    flags = ["--backend", args.backend]
    if args.lapack_library:
        flags += ["--lapack-library", str(args.lapack_library.resolve())]
    check(args.exe.resolve(), flags)
