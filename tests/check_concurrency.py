"""Check automatic/fixed worker selection and resume configuration."""
import json
from pathlib import Path
import re
import shutil
import tempfile

from check_cluster import command, recording
from check_features import require


def check_concurrency(exe):
    with tempfile.TemporaryDirectory(prefix="ltxk worker config ") as temporary:
        root = Path(temporary)
        isolated = root / exe.name
        shutil.copy2(exe, isolated)
        queue = root / "queue"
        config = root / "ltxk.json"
        def selected(*flags):
            result = command(isolated, "resume", "--queue-dir", queue, *flags)
            require(result.returncode == 0, result.stderr)
            match = re.search(r"with up to (\d+) jobs", result.stderr)
            require(match is not None, result.stderr)
            return int(match[1])
        automatic = selected()
        require(1 <= automatic <= 8, "Auto limit outside its conservative range")
        config.write_text('{"jobs":2}', encoding="utf-8")
        require(selected() == 2, "Resume ignored the configured worker limit")
        require(selected("--jobs", "auto") == automatic, "Explicit auto did not override fixed configuration")
        require(selected("--jobs", "16") == 16, "Explicit worker limit was not honored")
        config.write_text('{"jobs":"auto"}', encoding="utf-8")
        require(selected() == automatic, "Auto JSON configuration failed")
        source = root / "recording.1"
        recording(source, 200)
        result = command(isolated, "enqueue", source, "--queue-dir", queue, "--jobs", "2")
        require(result.returncode == 0, result.stderr)
        records = json.loads(command(isolated, "status", "--json", "--queue-dir", queue).stdout)
        require(len(records) == 1 and records[0]["settings"]["jobs"] == 2, "Queue did not retain the resolved setting")
        for value in ("0", "17", "many"):
            require(command(isolated, "resume", "--jobs", value, "--queue-dir", queue).returncode != 0,
                    f"Invalid worker limit accepted: {value}")
    print("Automatic/fixed worker limits, configuration, resume, and legacy job metadata passed")
