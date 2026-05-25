@echo off
REM ── Pachan Desktop Overlay Launcher ──────────────────────────────────────
REM Launches Pachan as a frameless Chrome window pinned to the right side
REM of your screen. The Python server must already be running (start.sh).

set CHROME="C:\Program Files\Google\Chrome\Application\chrome.exe"
if not exist %CHROME% set CHROME="C:\Program Files (x86)\Google\Chrome\Application\chrome.exe"
if not exist %CHROME% (
  echo Chrome not found. Edit this file and set the correct path.
  pause
  exit /b 1
)

REM Get screen width via PowerShell and place window at right edge
for /f %%W in ('powershell -command "[System.Windows.Forms.Screen]::PrimaryScreen.WorkingArea.Width"') do set SCR_W=%%W
if "%SCR_W%"=="" set SCR_W=1920 

set /a WIN_X=%SCR_W%-420

start "" %CHROME% ^
  --app=http://localhost:8000?overlay=1 ^
  --window-size=400,600 ^
  --window-position=%WIN_X%,60 ^
  --disable-background-timer-throttling ^
  --disable-renderer-backgrounding ^
  --autoplay-policy=no-user-gesture-required
