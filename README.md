# Pachan

Pachan is an always-on-top Live2D desktop companion built with Tauri. The web
frontend is retained because Tauri renders it inside its native webview; Electron
is no longer part of the application.

## Supported launch modes

- **Tauri (primary):** the complete desktop overlay, including tray controls,
  active-window awareness, screen vision, persistent memory, music control, and
  native window integration. Run `launch-tauri.bat` on Windows.
- **Browser (development):** starts the frontend and Python compatibility API at
  <http://127.0.0.1:8000>. Run `start.sh`. Native desktop features are unavailable.

## Configuration

Copy `.env.example` to `.env` while developing. Packaged builds can read `.env`
from the executable directory or Pachan's per-user config directory. At minimum,
configure `OLLAMA_HOST` and `OLLAMA_MODEL`; `OLLAMA_VISION_MODEL` enables screen
vision.

Pachan stores conversation history, remembered profile facts, and the YouTube
Music pairing token in its per-user application-data directory. These files can
contain personal information and should not be shared.

## Development

The browser server installs Python dependencies on first launch:

```sh
./start.sh
```

The Tauri launcher requires Rust and Tauri CLI on Windows:

```bat
launch-tauri.bat
```

Live2D model and overlay assets are intentionally ignored by Git and must be
present under `frontend/model` and `frontend/overlays` before packaging.
