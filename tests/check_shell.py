"""Exercise Explorer batch submission without requiring Explorer or certificate trust."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

from common import command, recording, require


def check_shell(exe):
    with tempfile.TemporaryDirectory(prefix="ltxk Explorer λ ") as temporary:
        root = Path(temporary)
        queue = root / "queue"
        request = root / "selection.json"
        sources = [root / f"recording & ! λ.{i}" for i in range(1, 33)]
        for source in sources:
            recording(source, 30)

        # First use must create the default queue without an older installation
        # having populated the profile. Override only the child's environment.
        profile = root / "fresh profile [literal] λ"
        environment = os.environ.copy()
        environment.update(HOME=str(profile), LOCALAPPDATA=str(profile), XDG_STATE_HOME=str(profile))
        default_queue = profile / ("Library/Application Support/ltxk/queue" if sys.platform == 'darwin'
                                   else "ltxk/queue")
        request.write_text(json.dumps([str(sources[0])]), encoding="utf-8")
        result = command(exe, "enqueue", "--request", request, env=environment)
        require(result.returncode == 0, result.stderr)
        require(len(list((default_queue / "pending").glob("*.json"))) == 1,
                "First submission did not create the default queue")

        missing_request = root / "missing folder" / "selection.json"
        result = command(exe, "enqueue", "--request", missing_request, "--queue-dir", queue)
        require(result.returncode != 0 and str(missing_request) in result.stderr
                and "Explorer selection" in result.stderr, result.stderr)

        def submit(value):
            request.write_text(json.dumps(value), encoding="utf-8")
            return command(exe, "enqueue", "--request", request, "--queue-dir", queue)
        result = submit(list(map(str, sources)))
        require(result.returncode == 0 and 'Queued 32 recordings' in result.stdout, result.stderr)
        require(len(list((queue / "pending").glob("*.json"))) == 32, "Selection was not queued as a batch")
        require(not list((queue / "done").glob("*.json")), "Queue-only submission unexpectedly processed recordings")
        result = submit(list(map(str, sources)))
        require(result.returncode == 0 and len(list((queue / "pending").glob("*.json"))) == 32, "Repeated selection was not deduplicated")
        large_selection = list(map(str, sources)) * 50
        require(len(json.dumps(large_selection)) > 32767, "Large selection did not exceed a Windows command line")
        result = submit(large_selection)
        require(result.returncode == 0 and len(list((queue / "pending").glob("*.json"))) == 32,
                "Request-file submission failed for a selection larger than a Windows command line")
        added = root / "later.33"
        recording(added, 30)
        sidecars = [root / f'recording.{kind}.1' for kind in ('clu', 'FET', 'res', 'spk')]
        for sidecar in sidecars:
            sidecar.write_bytes(b'1\n1\n')
        invalid_selections = [[], [str(added), 'relative.1'], [str(added), str(root / 'missing.4')]]
        invalid_selections += [[str(added), str(sidecar)] for sidecar in sidecars]
        for invalid in invalid_selections:
            result = submit(invalid)
            require(result.returncode != 0, f"Invalid selection accepted: {invalid}")
            require(len(list((queue / "pending").glob("*.json"))) == 32, "Invalid selection was partially queued")
        missing_recording = root / "missing folder" / "recording.4"
        result = submit([str(added), str(missing_recording)])
        require(result.returncode != 0 and str(missing_recording) in result.stderr
                and "recording" in result.stderr, result.stderr)
        require(len(list((queue / "pending").glob("*.json"))) == 32, "Missing recording was partially queued")

        blocked_queue = root / "not a queue folder"
        blocked_queue.write_text("keep", encoding="utf-8")
        request.write_text(json.dumps([str(added)]), encoding="utf-8")
        result = command(exe, "enqueue", "--request", request, "--queue-dir", blocked_queue)
        require(result.returncode != 0 and str(blocked_queue) in result.stderr, result.stderr)
        require(blocked_queue.read_text(encoding="utf-8") == "keep", "Submission changed an existing file")

        bad_record = queue / "pending" / "unreadable.json"
        bad_record.write_text("{broken json", encoding="utf-8")
        result = submit([str(added)])
        # Queue::open canonicalizes its root, expanding Windows short names
        # such as RUNNER~1 as well as resolving symlinks on other platforms.
        require(result.returncode != 0 and str(bad_record.resolve()) in result.stderr, result.stderr)
        bad_record.unlink()
        request.write_text('{broken json', encoding="utf-8")
        result = command(exe, "enqueue", "--request", request, "--queue-dir", queue)
        require(result.returncode != 0 and 'Explorer selection' in result.stderr, result.stderr)
        if os.name != 'nt':
            source = Path(__file__).resolve().parent.parent / "windows/test_shell.cpp"
            native = root / "test-shell"
            subprocess.run(['c++', '-std=c++17', '-Wall', '-Wextra', '-Werror', str(source), '-o', str(native)], check=True)
            subprocess.run([str(native)], check=True)
    print("Explorer first-use submission, path diagnostics, invalid selections, and duplicate checks passed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument('--exe', type=Path, required=True)
    check_shell(parser.parse_args().exe.resolve())
