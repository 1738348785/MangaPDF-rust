@echo off
set "PATH=D:\msys64\ucrt64\bin;%USERPROFILE%\.cargo\bin;%PATH%"
echo [MangaPDF] Starting Release Build...
cargo build --release
if %ERRORLEVEL% equ 0 (
    copy /y "target\release\pdf_tool.exe" "MangaPDF.exe"
    if exist "pdf_tool.exe" del /f /q "pdf_tool.exe"
    echo [MangaPDF] Build Succeeded: MangaPDF.exe is ready with embedded icon!
) else (
    echo [MangaPDF] Build Failed!
)
pause
