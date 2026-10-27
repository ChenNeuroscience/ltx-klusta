"""Exercise Explorer batch submission without requiring Explorer or certificate trust."""
import argparse
import json
import os
from pathlib import Path
import subprocess
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
        request.write_text('{broken json', encoding="utf-8")
        result = command(exe, "enqueue", "--request", request, "--queue-dir", queue)
        require(result.returncode != 0 and 'Explorer selection' in result.stderr, result.stderr)
        if os.name != 'nt':
            source = Path(__file__).resolve().parent.parent / "windows/test_shell.cpp"
            native = root / "test-shell"
            subprocess.run(['c++', '-std=c++17', '-Wall', '-Wextra', '-Werror', str(source), '-o', str(native)], check=True)
            subprocess.run([str(native)], check=True)
    print("Explorer batch submission, invalid selections, and duplicate checks passed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument('--exe', type=Path, required=True)
    check_shell(parser.parse_args().exe.resolve())
