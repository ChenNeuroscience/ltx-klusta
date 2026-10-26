"""Compare two ltxk builds on private recording copies; never replace user outputs."""
import argparse
from collections import Counter
import hashlib
from itertools import zip_longest
import json
import os
from pathlib import Path
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import time


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def agreement(reference, candidate):
    pairs, left, right = Counter(), Counter(), Counter()
    points = changed = 0
    with reference.open() as a, candidate.open() as b:
        counts = [int(a.readline()), int(b.readline())]
        for x, y in zip_longest(a, b):
            if x is None or y is None:
                raise RuntimeError("The builds assigned different numbers of spikes")
            x, y = int(x), int(y)
            points += 1
            changed += x != y
            pairs[x, y] += 1
            left[x] += 1
            right[y] += 1
    def combinations(n):
        return n * (n - 1) // 2
    total = combinations(points)
    index = sum(map(combinations, pairs.values()))
    a = sum(map(combinations, left.values()))
    b = sum(map(combinations, right.values()))
    expected = a * b / total if total else 0.
    denominator = (a + b) / 2 - expected
    return {"spikes": points, "changed_labels": changed,
            "adjusted_rand_index": (index - expected) / denominator if denominator else 1.,
            "signal_clusters": [c - 1 for c in counts], "noise_spikes": [left[1], right[1]]}


def measure(executable, source, config, log, timeout):
    environment = os.environ.copy()
    environment["LTXK_PROFILE"] = "1"
    command = [str(executable), "cluster", str(source), "--config", str(config),
               "--force", "--jobs", "1", "--backend", "nalgebra", "--features", "3PC",
               "--min-clusters", "20", "--max-clusters", "30", "--seed", "1",
               "--target-spikes", "25000", "--max-iterations", "10000", "--block-spikes", "4096"]
    started = time.perf_counter()
    with log.open("w", encoding="utf-8") as stream:
        result = subprocess.run(command, stdout=stream, stderr=stream, env=environment, timeout=timeout)
    elapsed = time.perf_counter() - started
    text = log.read_text(encoding="utf-8")
    if result.returncode:
        raise RuntimeError(f"{executable} exited {result.returncode}:\n{text}")
    phases = {name: sum(map(float, re.findall(r"\b" + name + r"=([\d.]+)", text)))
              for name in ("extract_s", "cache_s", "load_s", "fit_s", "assign_s")}
    trials = [{"initial": int(start), "clusters": int(clusters), "noise": int(noise), "score": float(score)}
              for start, clusters, noise, score in re.findall(
                  r"^Start (\d+): (\d+) signal clusters, (\d+) noise spikes, full score ([^\s]+)$", text, re.M)]
    if len(trials) != 11 or "Profile extract_s=" not in text:
        raise RuntimeError("Both builds must support the current CLI and LTXK_PROFILE output")
    output = source.with_suffix(".clu" + source.suffix)
    return {"wall_s": elapsed, **phases, "clustering_s": sum(phases[k] for k in ("load_s", "fit_s", "assign_s")),
            "sha256": digest(output), "trials": trials}, output


def benchmark(args):
    builds = {name: getattr(args, name).resolve(strict=True) for name in ("baseline", "candidate")}
    inputs = [p.resolve(strict=True) for p in args.recordings]
    protected = set(inputs) | set(builds.values()) | {p.with_suffix(".clu" + p.suffix) for p in inputs}
    if args.output.resolve() in protected:
        raise ValueError("Choose a report path separate from recordings, cluster outputs and executables")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    report = {"builds": {name: {"path": str(exe), "sha256": digest(exe)} for name, exe in builds.items()},
              "settings": {"features": "3PC", "backend": "nalgebra", "seed": 1, "jobs": 1,
                           "target_spikes": 25000, "initial_clusters": [20, 30], "block_spikes": 4096},
              "recordings": list(map(str, inputs)), "runs": [], "summaries": []}
    with tempfile.TemporaryDirectory(prefix="ltxk benchmark ") as temporary:
        root = Path(temporary)
        config = root / "settings.json"
        config.write_text(json.dumps({"work_dir": str(root)}), encoding="utf-8")
        copies = []
        for i, source in enumerate(inputs):
            folder = root / str(i)
            folder.mkdir()
            copy = folder / source.name
            shutil.copyfile(source, copy)
            copies.append(copy)
        for repeat in range(args.runs):
            for i, source in enumerate(copies):
                for name in (["baseline", "candidate"] if repeat % 2 == 0 else ["candidate", "baseline"]):
                    row, labels = measure(builds[name], source, config, source.parent / "run.log", args.timeout)
                    row.update(recording=i, build=name, repeat=repeat + 1)
                    reference = source.parent / "reference.clu"
                    if not reference.exists():
                        shutil.copyfile(labels, reference)
                    row["agreement"] = agreement(reference, labels)
                    report["runs"].append(row)
                    args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
                    print(f"{inputs[i].name}: {name} run {repeat + 1}: {row['wall_s']:.3f}s total, "
                          f"{row['clustering_s']:.3f}s clustering; "
                          f"{row['agreement']['changed_labels']} changed labels", flush=True)
        for i, source in enumerate(inputs):
            summary = {"recording": str(source)}
            for name in builds:
                rows = [r for r in report["runs"] if r["recording"] == i and r["build"] == name]
                summary[name] = {key: {"median": statistics.median(r[key] for r in rows),
                                       "min": min(r[key] for r in rows), "max": max(r[key] for r in rows)}
                                 for key in ("wall_s", "clustering_s", "fit_s", "assign_s")}
            report["summaries"].append(summary)
    args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(f"Wrote timings, output hashes, trial scores and label agreement to {args.output}")


if __name__ == "__main__":
    # Redirected Python output on Windows otherwise uses the legacy code page.
    sys.stdout.reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", required=True, type=Path)
    parser.add_argument("--candidate", required=True, type=Path)
    parser.add_argument("--recordings", required=True, nargs="+", type=Path)
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--timeout", type=float, default=1800, help="Seconds per clustering run")
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    if args.runs < 1 or args.timeout <= 0:
        parser.error("--runs and --timeout must be positive")
    benchmark(args)
