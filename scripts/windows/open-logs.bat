@echo off
setlocal EnableExtensions DisableDelayedExpansion
if not exist "%LOCALAPPDATA%\ltxk\queue\logs\" (
    echo No ltx-klusta queue logs have been created yet.
    pause
    exit /b 0
)
start "" "%SystemRoot%\explorer.exe" "%LOCALAPPDATA%\ltxk\queue\logs"
