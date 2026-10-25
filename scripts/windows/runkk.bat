@echo off
setlocal EnableExtensions DisableDelayedExpansion
title ltx-klusta queue
if not exist "%~dp0ltxk.exe" (
    echo ltxk.exe must be in the same folder as this script.
    pause
    exit /b 1
)
if "%~1"=="" (
    "%~dp0ltxk.exe" resume --console
) else (
    "%~dp0ltxk.exe" enqueue --console %*
)
exit /b %errorlevel%
