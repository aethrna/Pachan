const { app, BrowserWindow, screen, Tray, Menu, nativeImage } = require('electron');
const path = require('path');

let win;
let tray;

function createWindow() {
  const { width, height } = screen.getPrimaryDisplay().workAreaSize;

  win = new BrowserWindow({
    width: 400,
    height: 600,
    x: width - 420,
    y: 60,
    transparent: true,
    frame: false,
    alwaysOnTop: true,
    resizable: true,
    skipTaskbar: true,
    hasShadow: false,
    webPreferences: {
      nodeIntegration: false,
      contextIsolation: true,
    },
  });

  // Load with overlay flag so drag regions activate
  win.loadURL('http://localhost:8000?overlay=1');

  // Keep on top above fullscreen apps
  win.setAlwaysOnTop(true, 'screen-saver');
  win.setVisibleOnAllWorkspaces(true, { visibleOnFullScreen: true });

  win.on('closed', () => { win = null; });
}

function createTray() {
  // Blank 1x1 transparent icon as placeholder
  const icon = nativeImage.createEmpty();
  tray = new Tray(icon);
  tray.setToolTip('Pachan');

  const menu = Menu.buildFromTemplate([
    { label: 'Show Pachan',  click: () => win && win.show() },
    { label: 'Hide Pachan',  click: () => win && win.hide() },
    { type: 'separator' },
    { label: 'Quit',         click: () => app.quit() },
  ]);

  tray.setContextMenu(menu);
  tray.on('click', () => {
    if (!win) return;
    win.isVisible() ? win.hide() : win.show();
  });
}

app.whenReady().then(() => {
  createWindow();
  createTray();
});

app.on('window-all-closed', () => {
  // Stay in tray even when window is closed
});
