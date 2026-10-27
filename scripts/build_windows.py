"""Build the native Explorer extension with the Windows SDK, then sign its package.

Called by package_rust.py on Windows. No SDK or compiler is needed on users' PCs.
"""
import datetime
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
import zlib


ROOT = Path(__file__).resolve().parent.parent


def msvc_environment():
    locator = Path(os.environ["ProgramFiles(x86)"]) / "Microsoft Visual Studio/Installer/vswhere.exe"
    install = subprocess.check_output([str(locator), "-utf8", "-latest", "-products", "*", "-requires",
                                       "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
                                       "-property", "installationPath"], encoding="utf-8-sig").strip()
    if not install:
        raise RuntimeError("Visual Studio C++ build tools are required to build Explorer integration")
    setup = Path(install) / "Common7/Tools/VsDevCmd.bat"
    output = subprocess.check_output(["cmd.exe", "/d", "/u", "/s", "/c",
                                      f'call "{setup}" -arch=x64 -host_arch=x64 >nul && set'], encoding="utf-16-le")
    environment = os.environ.copy()
    for line in output.splitlines():
        if "=" in line and not line.startswith("="):
            name, value = line.split("=", 1)
            environment[name.upper()] = value
    return environment


def logo(size):
    """A small code-drawn L mark, encoded as PNG without imaging dependencies."""
    rows = bytearray()
    for y in range(size):
        rows.append(0)
        for x in range(size):
            white = ((size * 3 // 10 <= x < size * 4 // 10 and size // 4 <= y < size * 3 // 4)
                     or (size * 3 // 10 <= x < size * 7 // 10 and size * 13 // 20 <= y < size * 3 // 4))
            rows.extend((255, 255, 255, 255) if white else (34, 77, 108, 255))
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(bytes(rows))) + chunk(b"IEND", b""))


def build(output):
    environment = msvc_environment()
    output = output.resolve()
    assets = output / "Assets"
    assets.mkdir()
    for name, size in (("StoreLogo", 50), ("Square44x44Logo", 44), ("Square150x150Logo", 150)):
        (assets / f"{name}.png").write_bytes(logo(size))
    with tempfile.TemporaryDirectory(prefix="ltxk-shell-build-") as temporary:
        work = Path(temporary)
        image = logo(64)
        (work / "ltxk.ico").write_bytes(struct.pack("<HHH", 0, 1, 1)
                                      + struct.pack("<BBBBHHII", 64, 64, 0, 0, 1, 32, len(image), 22) + image)
        (work / "icon.rc").write_text(f'#pragma code_page(65001)\n1 ICON "{(work / "ltxk.ico").as_posix()}"\n', encoding="utf-8")
        def run(args):
            subprocess.run(args, check=True, cwd=work, env=environment)
        run(["rc.exe", "/nologo", "/fo", str(work / "icon.res"), str(work / "icon.rc")])
        compiler = ["cl.exe", "/nologo", "/std:c++17", "/W4", "/WX", "/EHsc", "/MT", "/utf-8",
                    "/DUNICODE", "/D_UNICODE", "/D_WIN32_WINNT=0x0A00", "/DWINVER=0x0A00"]
        libraries = ["shell32.lib", "shlwapi.lib", "ole32.lib", "oleaut32.lib", "user32.lib", "uuid.lib"]
        run([*compiler, "/LD", str(ROOT / "windows/explorer.cpp"),
             f"/Fe{output / 'ltxk-shell.dll'}", "/link", f"/IMPLIB:{work / 'ltxk-shell.lib'}",
             f"/DEF:{ROOT / 'windows/explorer.def'}", *libraries])
        run([*compiler, str(ROOT / "windows/launcher.cpp"), str(work / "icon.res"),
             f"/Fe{output / 'ltxk-launch.exe'}", "/link", "/SUBSYSTEM:WINDOWS", "/MANIFEST:EMBED",
             f"/MANIFESTINPUT:{ROOT / 'windows/launcher.manifest'}", *libraries, "uiautomationcore.lib"])
        run([*compiler, str(ROOT / "windows/test_shell.cpp"), f"/Fe{work / 'test-shell.exe'}",
             "/link", *libraries])
        run([str(work / "test-shell.exe"), str(output / "ltxk-shell.dll")])
        # Sparse packages contain the identity manifest; binaries/assets remain external.
        identity = work / "identity"
        identity.mkdir()
        now = datetime.datetime.now(datetime.timezone.utc)
        days = (now.date() - datetime.date(2020, 1, 1)).days
        tick = (now.hour * 3600 + now.minute * 60 + now.second) // 2
        version = f"1.0.{days}.{tick}"
        manifest = (ROOT / "windows/AppxManifest.xml.in").read_text(encoding="utf-8").replace("@VERSION@", version)
        (identity / "AppxManifest.xml").write_text(manifest, encoding="utf-8")
        (output / "AppxManifest.xml").write_text(manifest, encoding="utf-8")
        run(["makeappx.exe", "pack", "/o", "/nv", "/d", str(identity), "/p", str(output / "ltxk.msix")])
        run(["powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
             str(ROOT / "scripts/sign_windows.ps1"), "-PackageDirectory", str(output)])
    for name in ("Install Explorer integration.cmd", "Uninstall Explorer integration.cmd", "setup.ps1"):
        shutil.copy2(ROOT / "windows" / name, output / name)


if __name__ == "__main__":
    import argparse
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    build(parser.parse_args().output)
