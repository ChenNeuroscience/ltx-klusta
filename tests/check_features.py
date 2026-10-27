"""Feature mathematics via --dump: PCA, amplitude/VT, block sizes, Unicode paths and bad input."""
import argparse
import math
from pathlib import Path
import random
import tempfile

from common import command, dump_features, read_npy, require, write_waves


def variances(waves, channel=None):
    result = []
    for sample in range(50):
        values = [(w[channel] if channel is not None else w)[sample] for w in waves]
        mean = sum(values) / len(values)
        result.append(sum((x - mean) ** 2 for x in values))
    return result


def check_random_waves(exe, root):
    rng = random.Random(943)
    # Full-rank, non-degenerate signed data with different channel offsets.
    waves = [[[rng.randint(-100, 100) + channel for _ in range(50)] for channel in range(4)]
             for _ in range(80)]
    tetrode = root / "input with spaces.1"
    for active in (4, 2):
        write_waves(tetrode, waves, active)
        for components in ("A", "VT", "3PC", "3PC,A,VT"):
            rows = dump_features(exe, tetrode, root / "out", components)
            require(len(rows) == len(waves), "Missing feature rows")
            width = {"A": 1, "VT": 1, "3PC": 3, "3PC,A,VT": 5}[components]
            require(len(rows[0]) == active * width, "Wrong active channel count")
            if components in ("A", "VT"):
                for channel in range(active):
                    at = max(range(50), key=variances(waves, channel).__getitem__)
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
                            scale = math.sqrt(sum(x * x for x in column) * sum(x * x for x in previous))
                            require(abs(dot) / scale < 1e-5, "PCA orthogonality")
                shape, _ = read_npy(root / "out" / tetrode.name / "pca_components.npy")
                require(shape == (active, 3, 50), f"Wrong PCA component dump shape {shape}")


def check_analytic_pca(exe, root):
    tetrode = root / "data.1"
    rank_one = [[(i - 20) * ((j % 5) - 2) + 4 for j in range(50)] for i in range(41)]
    constant_wave = [[j - 25 for j in range(50)] for _ in range(41)]
    tied = [[a, b] + [0] * 48 for a, b in [(-7, 0), (0, -7), (7, 0), (0, 7)] * 20]
    cases = ((rank_one, "3PC,A,VT", lambda i, j: 100 * (i - 20) * (j - 20)),
             (constant_wave, "3PC,A,VT", lambda i, j: 0),
             (tied, "2PC", lambda i, j: sum(a * b for a, b in zip(tied[i], tied[j]))))
    for waves, components, expected_dot in cases:
        write_waves(tetrode, waves)
        original = tetrode.read_bytes()
        outputs = []
        for block in (1, 7, 4096):
            rows = dump_features(exe, tetrode, root / "out", components, ("--block-spikes", block))
            outputs.append(rows)
            pcs = int(components[0])
            for i in range(len(rows)):
                for j in range(len(rows)):
                    dot = sum(a * b for a, b in zip(rows[i][:pcs], rows[j][:pcs]))
                    require(math.isclose(dot, expected_dot(i, j), rel_tol=2e-5, abs_tol=1e-5),
                            "PCA does not preserve the known signal subspace")
            if components.endswith("A,VT"):
                at = max(range(50), key=variances(waves).__getitem__)
                require(all(row[pcs:] == [max(w) - min(w), w[at]] for w, row in zip(waves, rows)),
                        "A/VT values are incorrect")
        require(outputs[0] == outputs[1] == outputs[2], "Block size changed features")
        require(tetrode.read_bytes() == original, "Recording changed")
        unicode_root = root / "unicode λ"
        unicode_root.mkdir(exist_ok=True)
        unicode_input = unicode_root / "data λ.1"
        unicode_input.write_bytes(original)
        require(dump_features(exe, unicode_input, unicode_root / "out", components) == outputs[0],
                "Unicode paths changed extraction")


def check_bad_input(exe, root):
    tetrode = root / "bad.1"
    for count, message in ((-1, "at least 2"), (0, "at least 2"), (9000000, "shorter"),
                           (10000000, "shorter"), (2147483648, "2147483647")):
        tetrode.write_bytes(f"num_spikes {count}\r\ndata_start".encode())
        require(message in dump_features(exe, tetrode, root / "out", ok=False), "Wrong count error")
    for content in (b"num_spikes nonsense\ndata_start", b"num_spikes 80\n",
                    b"data_start", b"not a tetrode file"):
        tetrode.write_bytes(content)
        dump_features(exe, tetrode, root / "out", ok=False)
    result = command(exe, "cluster", root / "no-extension", "--no-queue")
    require(result.returncode != 0, "A recording without a numeric extension was accepted")


def check(exe):
    with tempfile.TemporaryDirectory(prefix="ltxk features ") as directory:
        root = Path(directory)
        check_random_waves(exe, root)
        check_analytic_pca(exe, root)
        check_bad_input(exe, root)
    print("Feature mathematics, block-size invariance, Unicode paths and input checks passed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", required=True, type=Path)
    check(parser.parse_args().exe.resolve())
