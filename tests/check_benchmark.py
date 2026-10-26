"""Smoke-test the portable benchmark helper without using real recordings."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

from check_cluster import recording
from check_features import require


def check_benchmark(exe):
    helper = Path(__file__).resolve().parents[1] / "scripts" / "benchmark_cluster.py"
    with tempfile.TemporaryDirectory(prefix="ltxk benchmark test ") as temporary:
        root = Path(temporary)
        source = root / "wave ! λ.1"
        recording(source, 1201)
        original = source.read_bytes()
        output = source.with_suffix(".clu.1")
        output.write_bytes(b"existing output")
        report = root / "report.json"
        command = [sys.executable, str(helper), "--baseline", str(exe), "--candidate", str(exe),
                   "--recordings", str(source), "--runs", "2", "--output", str(report)]
        environment = os.environ.copy()
        environment["PYTHONIOENCODING"] = "cp1252"
        result = subprocess.run(command, capture_output=True, text=True, encoding="utf-8", env=environment, timeout=60)
        require(result.returncode == 0, result.stdout + result.stderr)
        data = json.loads(report.read_text(encoding="utf-8"))
        require([r["build"] for r in data["runs"]] == ["baseline", "candidate", "candidate", "baseline"],
                "Build order must alternate")
        require(all(r["agreement"]["changed_labels"] == 0 and r["agreement"]["adjusted_rand_index"] == 1.
                    and len(r["trials"]) == 11 and r["clustering_s"] > 0 for r in data["runs"]),
                "Benchmark report omitted results or showed different output from the same executable")
        require(len(data["summaries"]) == 1, "Missing timing summary")
        require(source.read_bytes() == original and output.read_bytes() == b"existing output",
                "Benchmark touched original recordings or outputs")
        protected = subprocess.run([*command[:-1], str(source)], capture_output=True, timeout=10)
        require(protected.returncode != 0 and source.read_bytes() == original, "Report overwrote a recording")
    print("Benchmark helper: alternating builds, complete reports, label agreement, and output safety passed")
