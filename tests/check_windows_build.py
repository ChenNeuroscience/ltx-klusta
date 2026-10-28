"""Exercise the real cmd.exe batch handoff before the Windows compilation steps."""
import os
from pathlib import Path
import subprocess
import shutil
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "scripts"))
from build_windows import batch_environment, msvc_environment
from common import require


def check():
    require(os.name == 'nt', 'Run this check on Windows')
    before = os.environ.copy()
    with tempfile.TemporaryDirectory(prefix='ltxk build bootstrap ') as temporary:
        setup = Path(temporary) / 'tools with spaces & punctuation.cmd'
        setup.write_text('@echo off\nset "LTXK_BOOTSTRAP_CHECK=loaded"\nexit /b 0\n', encoding='ascii')
        environment = batch_environment(setup)
        require(environment.get('LTXK_BOOTSTRAP_CHECK') == 'loaded', 'Quoted batch path was not executed')
        require(os.environ == before, 'Batch setup changed the parent environment')
        setup.write_text('@echo off\nexit /b 7\n', encoding='ascii')
        try:
            batch_environment(setup)
        except subprocess.CalledProcessError as error:
            require(error.returncode == 7, 'Batch setup failure code was lost')
        else:
            raise RuntimeError('Failed batch setup was accepted')
    environment = msvc_environment()
    require(environment.get('VCTOOLSINSTALLDIR') and environment.get('WINDOWSSDKDIR'),
            'Visual Studio setup did not load the C++ tools and Windows SDK')
    # This proves the returned PATH works without displaying the environment,
    # which can contain private signing credentials on a release runner.
    compiler = shutil.which('cl.exe', path=environment['PATH'])
    require(compiler is not None, 'The loaded Visual Studio PATH does not contain cl.exe')
    subprocess.run([compiler, '/nologo', '/?'], env=environment,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
    print('Windows batch quoting, failure propagation, and Visual Studio environment checks passed')


if __name__ == '__main__':
    check()
