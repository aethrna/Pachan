@echo off
REM ── Pachan Tauri Overlay ─────────────────────────────────────────────────
REM Requires: Rust + cargo installed on Windows
REM First run will compile (~2 min). Subsequent runs are instant.
REM The Python server (start.sh) must be running first.

cd /d C:\VT\src-tauri

where cargo >nul 2>&1
if %errorlevel% neq 0 (
  echo Rust not found. Install from https://rustup.rs then re-run this script.
  pause
  exit /b 1
)

REM Install tauri-cli if missing
cargo tauri --version >nul 2>&1
if %errorlevel% neq 0 (
  echo Installing Tauri CLI...
  cargo install tauri-cli --version "^2" --locked
)

echo Starting Pachan overlay...
cargo tauri dev
