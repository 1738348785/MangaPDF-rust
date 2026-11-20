@echo off
setlocal
echo [MangaPDF] Starting Release Build...

cargo build --release
if %ERRORLEVEL% equ 0 (
    taskkill /f /im MangaPDF.exe >nul 2>&1
    taskkill /f /im MangaPDF-rust.exe >nul 2>&1
    taskkill /f /im pdf_tool.exe >nul 2>&1
    copy /y "target\release\pdf_tool.exe" "MangaPDF.exe"
    if errorlevel 1 (
        echo [MangaPDF] Failed to copy MangaPDF.exe! Please close any running instance.
        goto :end
    )
    copy /y "target\release\pdf_tool.exe" "MangaPDF-rust.exe"
    if errorlevel 1 (
        echo [MangaPDF] Failed to copy MangaPDF-rust.exe! Please close any running instance.
        goto :end
    )
    icacls "MangaPDF.exe" /setintegritylevel Medium >nul 2>&1
    icacls "MangaPDF-rust.exe" /setintegritylevel Medium >nul 2>&1
    if exist "pdf_tool.exe" del /f /q "pdf_tool.exe"
    echo [MangaPDF] Build Succeeded: MangaPDF.exe and MangaPDF-rust.exe are ready!
) else (
    echo [MangaPDF] Build Failed!
)
:end
endlocal
pause
