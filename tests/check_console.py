"""Exercise console handoff, live progress, packaged launchers, and a real terminal."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

from check_cluster import command, recording
from check_features import require


def wait_for(predicate, message, timeout=30):
    deadline = time.monotonic() + timeout
    while not predicate():
        require(time.monotonic() < deadline, message)
        time.sleep(0.05)


def records(queue):
    return [json.loads(p.read_text(encoding="utf-8")) for phase in ("pending", "running", "done", "failed")
            for p in (queue / phase).glob("*.json")]


def holder(queue):
    queue.mkdir()
    process = subprocess.Popen([sys.executable, "-u", "-c", """
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
""", str(queue / "coordinator.lock")], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               text=True, encoding="utf-8")
    require(process.stdout.readline().strip() == "locked", "Test coordinator could not lock")
    return process


def check_plain(exe, root):
    queue = root / "queue"
    source = root / "wave & ! λ.1"
    bad = root / "bad.2"
    recording(source, 1201)
    bad.write_bytes(b"malformed")
    flags = ["--console", "--queue-dir", str(queue), "--target-spikes", "250", "--min-clusters", "2", "--max-clusters", "4"]
    coordinator = holder(queue)
    with (root / "console.txt").open("w+", encoding="utf-8") as log:
        viewer = subprocess.Popen([str(exe), "enqueue", str(source), *flags], stdin=subprocess.DEVNULL,
                                  stdout=log, stderr=log)
        try:
            wait_for(lambda: "Following the existing" in (root / "console.txt").read_text(encoding="utf-8"),
                     "Visible console did not attach to coordinator")
            second = command(exe, "enqueue", bad, *flags)
            require(second.returncode == 0 and "existing ltx-klusta console" in second.stderr, second.stderr)
            coordinator.kill()
            coordinator.communicate(timeout=5)
            require(viewer.wait(timeout=60) == 1, "A failed recording must produce a failing console exit code")
        finally:
            if viewer.poll() is None:
                viewer.kill()
                viewer.wait()
            if coordinator.poll() is None:
                coordinator.kill()
                coordinator.communicate()
    output = (root / "console.txt").read_text(encoding="utf-8")
    for expected in ("Start 2:", "FAILED", "Queue finished:", "1 failed", "elapsed", "Log:"):
        require(expected in output, f"Console omitted {expected}: {output}")
    require("\x1b[?1049h" not in output, "Redirected output should stay plain")
    jobs = records(queue)
    require(len(jobs) == 2 and all(j["finished_ms"] >= j["started_ms"] for j in jobs), "Missing job timing")
    good = next(j for j in jobs if j["phase"] == "done")
    progress = json.loads((queue / "progress" / (good["id"] + ".json")).read_text())
    require(progress["attempt_ms"] == good["started_ms"] and progress["trial"] == progress["trials"] == 3,
            "Missing structured trial progress")
    require(progress["spikes"] == 1201 and progress["fitting_spikes"] == 300, "Progress changed subset policy")
    # Observing the queue must preserve the clustering result.
    original = source.with_suffix(".clu.1").read_bytes()
    direct = command(exe, "cluster", source, "--force", *flags[3:])
    require(direct.returncode == 0 and source.with_suffix(".clu.1").read_bytes() == original, direct.stderr)
    # Completed history, including old failures, must not poison the next session.
    skipped = command(exe, "enqueue", source, *flags)
    require(skipped.returncode == 0 and "Skipped" in skipped.stderr and "0 failed" in skipped.stderr, skipped.stderr)
    conflict = command(exe, "enqueue", source, "--console", "--background")
    require(conflict.returncode != 0, "Conflicting console/background flags accepted")


def check_terminal(exe, root):
    if os.name == "nt":
        return  # Headless Windows uses the plain path; layout is also covered by TestBackend.
    import fcntl
    import pty
    import select
    import struct
    import termios
    queue = root / "terminal-queue"
    source = root / "terminal.1"
    recording(source, 200)
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 110, 0, 0))
    environment = os.environ.copy()
    environment["TERM"] = "xterm-256color"
    process = subprocess.Popen([str(exe), "enqueue", str(source), "--console", "--queue-dir", str(queue),
                                "--min-clusters", "2", "--max-clusters", "3"],
                               stdin=slave, stdout=slave, stderr=slave, env=environment)
    os.close(slave)
    captured = bytearray()
    def read():
        if select.select([master], [], [], 0.05)[0]:
            try:
                captured.extend(os.read(master, 65536))
            except OSError:
                pass
    try:
        def complete():
            read()
            return bool(list((queue / "done").glob("*.json")))
        wait_for(complete, "Interactive queue did not complete")
        def released():
            read()
            with (queue / "console.lock").open("a+b") as lock:
                try:
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    return True
                except BlockingIOError:
                    return False
        wait_for(released, "Completed dashboard did not release its console lease")
        # The finished dashboard must remain open while a fresh launch obtains
        # its own console lease. This exercises the idle/submit handoff barrier.
        second = command(exe, "enqueue", source, "--console", "--queue-dir", queue)
        require(second.returncode == 0 and "Queue finished:" in second.stderr, second.stderr)
        for _ in range(10):
            read()
        require(process.poll() is None, "Dashboard closed before the user could inspect completion")
        os.write(master, b"\r")
        wait_for(lambda: (read(), process.poll() is not None)[1], "Enter did not close the completed dashboard")
        require(process.returncode == 0 and b"\x1b[?1049h" in captured and b"\x1b[?1049l" in captured,
                f"Dashboard did not enter and restore the terminal: {captured!r}")
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
        os.close(master)


def check_windows_launcher(exe, root):
    if os.name != "nt":
        return
    launcher = exe.with_name("runkk.bat")
    require(launcher.is_file() and exe.with_name("open-logs.bat").is_file(), "Windows helpers missing from package")
    source = root / "dropped & ! λ.1"
    recording(source, 200)
    environment = os.environ.copy()
    environment["LOCALAPPDATA"] = str(root / "local app data")
    comspec = str(Path(os.environ["SystemRoot"]) / "System32" / "cmd.exe")
    # Pass cmd its native command-line syntax, including the outer /s /c quotes.
    result = subprocess.run(f'"{comspec}" /d /v:off /s /c ""{launcher}" "{source}""',
                            stdin=subprocess.DEVNULL, capture_output=True, env=environment, timeout=60)
    require(result.returncode == 0 and source.with_suffix(".clu.1").is_file(), repr(result))


def check_console(exe):
    with tempfile.TemporaryDirectory(prefix="ltxk console ") as temporary:
        root = Path(temporary)
        check_plain(exe, root)
        check_terminal(exe, root)
        check_windows_launcher(exe, root)
    print("Console handoff, live progress, timing, terminal restoration, and packaged launcher checks passed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", required=True, type=Path)
    check_console(parser.parse_args().exe.resolve())
