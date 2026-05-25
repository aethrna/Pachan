#!/bin/bash
set -e
cd "$(dirname "$0")"

# Install dependencies if needed
if ! python3 -c "import fastapi, httpx, youtubesearchpython" 2>/dev/null; then
  echo "Installing Python dependencies..."
  pip install -r backend/requirements.txt
fi

echo ""
echo "  Pachan AI Live2D"
echo "  ─────────────────────────────"
echo "  Local:  http://localhost:8000"
echo ""
echo "  For desktop overlay (run in Windows cmd/PowerShell):"
echo '  "C:\Program Files\Google\Chrome\Application\chrome.exe" --app=http://localhost:8000 --window-size=400,600 --window-position=1500,100'
echo ""

uvicorn backend.main:app --host 0.0.0.0 --port 8000 --reload
