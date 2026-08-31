$ErrorActionPreference = "Stop"

Write-Host "=========================================" -ForegroundColor Cyan
Write-Host "  MangaPDF - Release Build Script" -ForegroundColor Cyan
Write-Host "=========================================" -ForegroundColor Cyan

$env:PATH = "D:\msys64\ucrt64\bin;$env:USERPROFILE\.cargo\bin;$env:PATH"

Write-Host "`n[1/2] Building release binary with icon resource..." -ForegroundColor Yellow
cargo build --release

if ($LASTEXITCODE -eq 0) {
    Write-Host "`n[2/2] Deploying MangaPDF.exe..." -ForegroundColor Yellow
    Get-Process MangaPDF, pdf_tool -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 300
    Copy-Item "target\release\pdf_tool.exe" "MangaPDF.exe" -Force
    if (Test-Path "pdf_tool.exe") {
        Remove-Item "pdf_tool.exe" -Force
    }
    Write-Host "`n[SUCCESS] Build and Deployment Succeeded!" -ForegroundColor Green
    Write-Host "Executable is at: $PWD\MangaPDF.exe" -ForegroundColor Green
} else {
    Write-Host "`n[ERROR] Build Failed!" -ForegroundColor Red
}
