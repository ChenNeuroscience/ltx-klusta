"""Build the native Explorer extension with the Windows SDK, then sign its package.

Called by package_rust.py on Windows. No SDK or compiler is needed on users' PCs.
"""
import datetime
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parent.parent


def batch_environment(setup):
    # cmd.exe does not use the CRT argv quoting that subprocess applies to a
    # list. Pass its command line verbatim; /s removes the outer /c quotes and
    # leaves the quotes around the batch path intact (including Program Files).
    interpreter = os.environ.get("COMSPEC", "cmd.exe")
    command = f'call "{setup}" -arch=x64 -host_arch=x64 >nul && set'
    output = subprocess.check_output(f'"{interpreter}" /d /u /s /c "{command}"', encoding="utf-16-le")
    environment = os.environ.copy()
    for line in output.splitlines():
        if "=" in line and not line.startswith("="):
            name, value = line.split("=", 1)
            environment[name.upper()] = value
    return environment


def msvc_environment():
    locator = Path(os.environ["ProgramFiles(x86)"]) / "Microsoft Visual Studio/Installer/vswhere.exe"
    install = subprocess.check_output([str(locator), "-utf8", "-latest", "-products", "*", "-requires",
                                       "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
                                       "-property", "installationPath"], encoding="utf-8-sig").strip()
    if not install:
        raise RuntimeError("Visual Studio C++ build tools are required to build Explorer integration")
    return batch_environment(Path(install) / "Common7/Tools/VsDevCmd.bat")


def build(output):
    environment = msvc_environment()
    output = output.resolve()
    assets = output / "assets"
    shutil.copytree(ROOT / "assets/windows", assets, dirs_exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="ltxk-shell-build-") as temporary:
        work = Path(temporary)
        shutil.copy2(ROOT / "assets/ltxk.ico", work / "ltxk.ico")
        (work / "icon.rc").write_text(f'#pragma code_page(65001)\n1 ICON "{(work / "ltxk.ico").as_posix()}"\n', encoding="utf-8")
        def run(args):
            # On Windows CreateProcess searches the parent's PATH, not the PATH
            # supplied in env. Resolve tools against the loaded SDK environment.
            executable = shutil.which(args[0], path=environment["PATH"])
            if executable is None:
                raise RuntimeError(f"Build tool was not found in the Visual Studio environment: {args[0]}")
            subprocess.run([executable, *args[1:]], check=True, cwd=work, env=environment)
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
