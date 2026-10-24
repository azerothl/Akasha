@echo off
setlocal

echo Installation d'Akasha
echo.
powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\setup.ps1" %*
set "akashaInstallExitCode=%ERRORLEVEL%"

if not "%akashaInstallExitCode%"=="0" (
    echo.
    echo L'installation a echoue avec le code %akashaInstallExitCode%.
    echo Consultez le message PowerShell ci-dessus.
)

echo.
pause
exit /b %akashaInstallExitCode%
