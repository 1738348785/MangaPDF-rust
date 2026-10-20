@echo off
setlocal
echo [MangaPDF] Starting Release Build...

cargo build --release
if %ERRORLEVEL% equ 0 (
    copy /y "target\release\pdf_tool.exe" "MangaPDF.exe"
    copy /y "target\release\pdf_tool.exe" "MangaPDF-rust.exe"
    if exist "pdf_tool.exe" del /f /q "pdf_tool.exe"
    echo [MangaPDF] Build Succeeded: MangaPDF.exe and MangaPDF-rust.exe are ready!
) else (
    echo [MangaPDF] Build Failed!
)
endlocal
pause
