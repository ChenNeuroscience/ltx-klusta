"""Build ltx-klusta.app: a Finder droplet that contains ltxk and shows the queue in Terminal."""
import argparse
from pathlib import Path
import plistlib
import shutil
import subprocess
import tempfile

SCRIPT = '''on run
    set answer to display dialog ("ltx-klusta" & return & "Choose recordings to cluster, or open the queue to see progress. Recordings that already have a .clu file are skipped.") buttons {"Cancel", "Open Queue", "Choose Recordings"} default button "Choose Recordings" cancel button "Cancel"
    if button returned of answer is "Open Queue" then
        my showQueue({})
    else
        set recordings to choose file with prompt "Choose tetrode recordings (for example session.1)" with multiple selections allowed
        my showQueue(recordings)
    end if
end run

on open recordings
    my showQueue(recordings)
end open

on showQueue(recordings)
    set commandText to quoted form of (POSIX path of (path to resource "ltxk"))
    repeat with recording in recordings
        set commandText to commandText & " " & quoted form of (POSIX path of recording)
    end repeat
    tell application "Terminal"
        activate
        do script commandText
    end tell
end showQueue
'''


def create(executable, output):
    executable = executable.resolve(strict=True)
    output = output.absolute()
    if output.exists():
        raise FileExistsError(f"Choose a new output path: {output}")
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="ltxk app ") as folder:
        script = Path(folder) / "launcher.applescript"
        script.write_text(SCRIPT, encoding="utf-8")
        subprocess.run(["/usr/bin/osacompile", "-o", str(output), str(script)], check=True)
    shutil.copy2(executable, output / "Contents" / "Resources" / "ltxk")
    info = output / "Contents" / "Info.plist"
    with info.open("rb") as stream:
        metadata = plistlib.load(stream)
    metadata.update({
        "CFBundleIdentifier": "org.chenneuroscience.ltxk",
        "CFBundleName": "ltx-klusta",
        "CFBundleDisplayName": "ltx-klusta",
        "NSAppleEventsUsageDescription": "Show clustering progress in Terminal.",
        "CFBundleDocumentTypes": [{"CFBundleTypeName": "Tetrode recordings", "CFBundleTypeRole": "Viewer",
                                   "CFBundleTypeExtensions": ["*"], "LSHandlerRank": "None"}],
    })
    with info.open("wb") as stream:
        plistlib.dump(metadata, stream)
    # Adding files and changing the plist invalidate osacompile's ad-hoc signature.
    subprocess.run(["/usr/bin/codesign", "--force", "--deep", "--sign", "-", str(output)], check=True)
    print(f"Created {output}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    create(args.exe, args.output)
