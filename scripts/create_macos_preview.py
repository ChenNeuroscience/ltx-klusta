"""Build a local Finder droplet that opens the queue console in Terminal."""
import argparse
import json
from pathlib import Path
import plistlib
import subprocess
import tempfile


def create(executable, output):
    executable = executable.resolve(strict=True)
    output = output.absolute()
    if output.exists():
        raise FileExistsError(f"Choose a new output path: {output}")
    output.parent.mkdir(parents=True, exist_ok=True)
    # JSON quoting is used only as an AppleScript string literal, never as a
    # shell command. AppleScript's quoted form handles each shell argument.
    literal = json.dumps(str(executable), ensure_ascii=False)
    source = f'''property executablePath : {literal}

on run
    set answer to display dialog ("ltx-klusta queue preview" & return & "Choose recordings to cluster, or open the existing queue. Existing outputs are skipped.") buttons {{"Cancel", "Open Queue", "Choose Recordings"}} default button "Choose Recordings" cancel button "Cancel"
    if button returned of answer is "Open Queue" then
        my launchQueue({{}})
    else
        set recordings to choose file with prompt "Choose tetrode recordings (numeric extensions)" with multiple selections allowed
        my launchQueue(recordings)
    end if
end run

on open recordings
    my launchQueue(recordings)
end open

on launchQueue(recordings)
    set commandText to quoted form of executablePath
    if (count recordings) is 0 then
        set commandText to commandText & " resume --console"
    else
        set commandText to commandText & " enqueue --console"
        repeat with recording in recordings
            set commandText to commandText & " " & quoted form of (POSIX path of recording)
        end repeat
    end if
    tell application "Terminal"
        activate
        do script commandText
    end tell
end launchQueue
'''
    with tempfile.TemporaryDirectory(prefix="ltxk app ") as folder:
        script = Path(folder) / "launcher.applescript"
        script.write_text(source, encoding="utf-8")
        subprocess.run(["/usr/bin/osacompile", "-o", str(output), str(script)], check=True)
    info = output / "Contents" / "Info.plist"
    with info.open("rb") as stream:
        metadata = plistlib.load(stream)
    metadata.update({
        "CFBundleIdentifier": "org.chenneuroscience.ltxk.queue-preview",
        "CFBundleDisplayName": "ltx-klusta Preview",
        "NSAppleEventsUsageDescription": "Open the ltx-klusta queue console in Terminal.",
        "CFBundleDocumentTypes": [{"CFBundleTypeName": "Tetrode recordings", "CFBundleTypeRole": "Viewer", "CFBundleTypeExtensions": ["*"], "LSHandlerRank": "None"}],
    })
    with info.open("wb") as stream:
        plistlib.dump(metadata, stream)
    # Changing the plist invalidates osacompile's ad-hoc signature.
    subprocess.run(["/usr/bin/codesign", "--force", "--sign", "-", str(output)], check=True)
    print(f"Created {output}; uses {executable}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    create(args.exe, args.output)
