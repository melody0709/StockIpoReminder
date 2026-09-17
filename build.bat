@echo off
setlocal EnableExtensions EnableDelayedExpansion
cd /d "%~dp0"

set "MODE=Runtime"
set "CLEAN_FIRST=0"
set "CLEAN_ONLY=0"
set "SIGN_RELEASE=0"

:parse
if "%~1"=="" goto execute
if /I "%~1"=="--clean" (
    set "CLEAN_ONLY=1"
    shift
    goto parse
)
if /I "%~1"=="--rebuild" (
    set "CLEAN_FIRST=1"
    shift
    goto parse
)
if /I "%~1"=="--package" (
    set "MODE=All"
    shift
    goto parse
)
if /I "%~1"=="--package-msi" (
    set "MODE=Msi"
    shift
    goto parse
)
if /I "%~1"=="--package-portable" (
    set "MODE=Portable"
    shift
    goto parse
)
if /I "%~1"=="--sign" (
    set "SIGN_RELEASE=1"
    shift
    goto parse
)
if /I "%~1"=="--help" goto usage
if /I "%~1"=="-h" goto usage
echo ERROR: Unknown option: %~1
goto usage_error

:execute
if "!CLEAN_FIRST!"=="1" call :clean
if errorlevel 1 exit /b !ERRORLEVEL!
if "!CLEAN_ONLY!"=="1" (
    call :clean
    exit /b !ERRORLEVEL!
)

set "SKIP_TESTS="
if /I "!MODE!"=="Runtime" set "SKIP_TESTS=-SkipTests"
set "SIGN_ARGS="
if "!SIGN_RELEASE!"=="1" set "SIGN_ARGS=-Sign"
pwsh -NoProfile -ExecutionPolicy Bypass -File "%CD%\scripts\build-release.ps1" -PackageMode "!MODE!" !SKIP_TESTS! !SIGN_ARGS!
if errorlevel 1 exit /b !ERRORLEVEL!

echo Build Success
echo Runnable: %CD%\build\run\x64-release\StockIpoReminder.exe
if /I not "!MODE!"=="Runtime" echo Packages: %CD%\build\packages
exit /b 0

:clean
pwsh -NoProfile -ExecutionPolicy Bypass -File "%CD%\scripts\clean-build.ps1"
exit /b !ERRORLEVEL!

:usage
echo Usage: build.bat [--clean^|--rebuild] [--package^|--package-msi^|--package-portable] [--sign]
echo   default             Incremental release build and exact runtime install
echo   --clean             Remove generated compile/runtime/test/log trees; preserve packages
echo   --rebuild           Clean generated trees, then build runtime
echo   --package           Build, test, and create MSI plus Portable ZIP
echo   --package-msi       Build, test, and create MSI only
echo   --package-portable  Build, test, and create Portable ZIP only
echo   --sign              Optional Authenticode signing of EXE/MSI using configured credentials
echo.
echo In-app auto-update trust is always compiled in via the repository Minisign public
echo key; the signed update manifest is produced separately by
echo   pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/sign-update-manifest.ps1
exit /b 0

:usage_error
call :usage
exit /b 2
