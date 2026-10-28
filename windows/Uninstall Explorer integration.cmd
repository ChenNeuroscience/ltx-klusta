@echo off
"%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -ExecutionPolicy Bypass -File "%~dp0setup.ps1" -Action Uninstall
if errorlevel 1 echo Uninstallation failed. Read the message above before closing this window.
pause
