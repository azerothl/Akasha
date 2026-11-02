@echo off
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\consumer-launch.ps1" -RecoverProfile
pause
