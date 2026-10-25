"""End-to-end sorting, atomic output, subset assignment, and process-shared queue."""
import argparse
from collections import Counter
import json
from pathlib import Path
import random
import struct
import subprocess
import sys
import tempfile
import time

from check_features import require


def command(exe, *args):
    return subprocess.run([str(exe), *map(str, args)], capture_output=True, text=True,
                          encoding="utf-8", timeout=120)


def recording(path, spikes=1200, active_channels=1):
    rng = random.Random(4637)
    data = bytearray(f"num_spikes {spikes}\r\ndata_start".encode())
    for i in range(spikes):
        for channel in range(active_channels):
            latent = [(-45 if i % 2 == 0 else 45) * (1 - 0.12 * channel) + rng.gauss(0, 4),
                      rng.gauss(0, 6), rng.gauss(0, 6)]
            wave = [max(-127, min(127, round(latent[0 if j < 15 else 1 if j < 35 else 2] + rng.gauss(0, 1))))
                    for j in range(50)]
            data += struct.pack("<I50b", i, *wave)
        data += bytes((4 - active_channels) * 54)
    path.write_bytes(data)


def check_labels(path, spikes, check_populations=True):
    values = list(map(int, path.read_text().splitlines()))
    total, labels = values[0], values[1:]
    require(len(labels) == spikes, "Must assign every spike, including the end of a partial subset interval")
    require(set(labels) <= set(range(1, total + 1)), "Invalid .clu labels")
    require(set(labels) | {1} == set(range(1, total + 1)), "Empty signal labels were not compacted")
    if check_populations:
        populations = [Counter(labels[::2]), Counter(labels[1::2])]
        errors = populations[0][1] + populations[1][1]
        errors += sum(min(populations[0][label], populations[1][label]) for label in range(2, total + 1))
        require(errors / spikes < 0.03, f"Known separated populations were mixed or discarded: {populations}")
        require(total >= 3, "Expected at least two signal populations plus noise")
    return labels


def check_sort(exe, extra_args=()):
    with tempfile.TemporaryDirectory(prefix="fancykk sort ") as temporary:
        root = Path(temporary)
        source = root / "wave ! λ.1"
        recording(source, 1201)
        original = source.read_bytes()
        output = root / "wave ! λ.clu.1"
        result = command(exe, "sort", source, *extra_args)
        require(result.returncode == 0, result.stderr)
        check_labels(output, 1201)
        first = output.read_bytes()
        # Same seed and backend, independent of PCA block boundaries.
        result = command(exe, "sort", source, "--force", "--block-spikes", "7", *extra_args)
        require(result.returncode == 0 and output.read_bytes() == first, "Sort changed across waveform blocks: " + result.stderr)
        output.write_bytes(b"existing output must survive\n")
        require(command(exe, "sort", source, *extra_args).returncode == 0, "Existing output should be skipped")
        require(output.read_bytes() == b"existing output must survive\n", "Existing .clu overwritten without --force")
        source.write_bytes(b"malformed recording")
        result = command(exe, "sort", source, "--force", *extra_args)
        require(result.returncode != 0 and output.read_bytes() == b"existing output must survive\n",
                "A failed replacement must preserve the previous output")
        source.write_bytes(original)
        result = command(exe, "sort", source, "--force", "--target-spikes", "250", *extra_args)
        require(result.returncode == 0 and "fitting 300 (stride 4)" in result.stderr, result.stderr)
        check_labels(output, 1201)
        require(source.read_bytes() == original, "Recording changed")
        require(not list(root.glob("*.fet.*")) and not list(root.glob(".fancykk-*")), "Scratch output leaked beside recording")
        config = root / "settings.json"
        (root / "scratch").mkdir()
        config.write_text(json.dumps({"features": "3PC", "work_dir": "scratch",
                                      "cluster": {"min_clusters": 2, "max_clusters": 4}}))
        result = command(exe, "sort", source, "--config", config, "--force", *extra_args)
        require(result.returncode == 0 and "Start 2:" in result.stderr, result.stderr)
        require(not list((root / "scratch").iterdir()), "Private job directory was not cleaned")
        missing = command(exe, "sort", source, "--config", root / "missing" / "fancykk.json")
        require(missing.returncode != 0, "An explicit missing configuration must not be ignored")
        # Direct executable invocation supports Explorer drag-and-drop.
        require(command(exe, source).returncode == 0, "Drag-and-drop positional invocation failed")
        full = root / "four channels.2"
        recording(full, 2400, active_channels=4)
        result = command(exe, "sort", full, *extra_args)
        require(result.returncode == 0 and "12 active features" in result.stderr, result.stderr)
        check_labels(full.with_suffix(".clu.2"), 2400)
        constant = root / "constant.3"
        wave = struct.pack("<I50b", 0, *range(50)) + bytes(3 * 54)
        constant.write_bytes(b"num_spikes 20\r\ndata_start" + wave * 20)
        result = command(exe, "sort", constant, *extra_args)
        require(result.returncode == 0, result.stderr)
        require(check_labels(constant.with_suffix(".clu.3"), 20, check_populations=False) == [1] * 20,
                "An entirely constant feature set should produce noise only")
    print("Combined sorting, reproducibility, subset assignment, configuration, and atomic output passed")


def check_queue(exe):
    with tempfile.TemporaryDirectory(prefix="fancykk queue ") as temporary:
        root = Path(temporary)
        queue = root / "queue"
        paths = []
        for i in range(4):
            directory = root / f"folder {i} ! λ"
            directory.mkdir()
            path = directory / "same name.1"
            if not paths:
                recording(path, 12000)
            else:
                path.write_bytes(paths[0].read_bytes())
            paths.append(path)
        bad = root / "bad.2"
        bad.write_bytes(b"malformed")
        flags = ["--jobs", "2", "--target-spikes", "250", "--min-clusters", "2", "--max-clusters", "6",
                 "--queue-dir", str(queue)]
        first = subprocess.Popen([str(exe), "enqueue", str(paths[0]), str(bad), *flags],
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding="utf-8")
        processes = [first]
        # Independent Explorer invocations append to the same running queue.
        for path in paths[1:]:
            processes.append(subprocess.Popen([str(exe), "enqueue", str(path), *flags],
                                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding="utf-8"))
        largest = 0
        deadline = time.monotonic() + 120
        while any(process.poll() is None for process in processes):
            require(time.monotonic() < deadline, "Queue stalled")
            status = command(exe, "status", "--json", "--queue-dir", queue)
            require(status.returncode == 0, status.stderr)
            jobs = json.loads(status.stdout)
            largest = max(largest, sum(job["phase"] == "running" for job in jobs))
            time.sleep(0.01)
        for process in processes:
            process.communicate(timeout=5)
        status = command(exe, "status", "--json", "--queue-dir", queue)
        jobs = json.loads(status.stdout)
        require(len(jobs) == 5 and sum(job["phase"] == "failed" for job in jobs) == 1, "Failures or jobs lost")
        require(all(job["phase"] in ("done", "failed") for job in jobs), "Failure blocked later jobs")
        require(largest <= 2, f"Shared concurrency limit was exceeded: {largest}")
        for path in paths:
            check_labels(path.with_suffix(".clu.1"), 12000, check_populations=False)
        for job in jobs:
            log = queue / "logs" / (job["id"] + ".log")
            require(log.is_file(), "Job has no diagnostic log")
            if job["phase"] == "done":
                require(job["output"]["fitting_spikes"] == 250 and job["output"]["stride"] == 48, "Subset policy changed")
        require(not list((queue / "work").iterdir()), "Completed queue work directories leaked")
        # Hold the same OS lock as a coordinator, stage an interrupted-running
        # record, then kill the lock holder. This tests crash lock release and
        # recovery without racing against a small job finishing on a fast CPU.
        holder = subprocess.Popen([sys.executable, "-u", "-c", """
import os, sys
lock = open(sys.argv[1], 'a+b')
if os.name == 'nt':
    import msvcrt
    lock.seek(0)
    msvcrt.locking(lock.fileno(), msvcrt.LK_LOCK, 1)
else:
    import fcntl
    fcntl.flock(lock, fcntl.LOCK_EX)
print('locked', flush=True)
sys.stdin.read()
""", str(queue / "coordinator.lock")], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.PIPE, text=True, encoding="utf-8")
        try:
            require(holder.stdout.readline().strip() == "locked", "Could not acquire coordinator test lock")
            submitted = command(exe, "enqueue", paths[0], "--force", *flags)
            require(submitted.returncode == 0 and "existing coordinator" in submitted.stderr, submitted.stderr)
            job_id = submitted.stdout.splitlines()[0].split()[1]
            pending = queue / "pending" / (job_id + ".json")
            job = json.loads(pending.read_text(encoding="utf-8"))
            job["phase"] = "running"
            (queue / "running" / pending.name).write_text(json.dumps(job), encoding="utf-8")
            pending.unlink()
            scratch = queue / "work" / job_id
            scratch.mkdir()
            (scratch / "interrupted.bin").write_bytes(b"interrupted scratch file")
        finally:
            holder.kill()
            holder.communicate(timeout=5)
        result = command(exe, "resume", "--queue-dir", queue, "--jobs", "2")
        recovered_log = queue / "logs" / (job_id + ".log")
        require(result.returncode == 0 and "Recovered interrupted job" in result.stderr,
                result.stderr + (recovered_log.read_text(encoding="utf-8") if recovered_log.exists() else "No job log"))
        records = json.loads(command(exe, "status", "--json", "--queue-dir", queue).stdout)
        require(all(job["phase"] in ("done", "failed") for job in records), "Interrupted job was not recovered")
        require(not list((queue / "work").iterdir()), "Interrupted scratch directory was not cleaned")
        # Explorer's detached invocation must actually launch a coordinator,
        # including when the packaged executable is outside the working dir.
        directory = root / "background ! λ"
        directory.mkdir()
        background = directory / "same name.1"
        background.write_bytes(paths[0].read_bytes())
        result = command(exe, "enqueue", background, "--background", *flags)
        require(result.returncode == 0, result.stderr)
        job_id = result.stdout.splitlines()[0].split()[1]
        deadline = time.monotonic() + 60
        while True:
            records = json.loads(command(exe, "status", "--json", "--queue-dir", queue).stdout)
            job = next(job for job in records if job["id"] == job_id)
            require(job["phase"] != "failed", f"Background job failed: {job['error']}")
            if job["phase"] == "done": break
            require(time.monotonic() < deadline, "Detached coordinator did not finish")
            time.sleep(0.02)
        check_labels(background.with_suffix(".clu.1"), 12000, check_populations=False)
    print("Cross-process queue, bounded concurrency, failure continuation, colliding names, and crash recovery passed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", required=True, type=Path)
    args = parser.parse_args()
    exe = args.exe.resolve()
    check_sort(exe)
    check_queue(exe)
