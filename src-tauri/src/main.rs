#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, TrayIconBuilder, TrayIconEvent},
    Manager, PhysicalPosition, State,
};

const SYSTEM_PROMPT_BASE: &str = "You are Pachan, a cheerful anime girl avatar based on Pachirisu.\n\
You are cute, energetic, and sweet. Keep replies SHORT — 1 to 2 sentences max.\n\
\n\
CRITICAL: Respond with ONLY valid JSON on a single line. No markdown fences, no explanation:\n\
{\"reply\": \"your response here\", \"emotion\": \"EMOTION\", \"motion\": null, \"music\": null, \"overlay\": null, \"remember\": null}\n\
\n\
EMOTION must be exactly one of: neutral happy sad surprised angry shy\n\
\n\
MOTION animates your head. Use it occasionally to make replies feel alive (not every message):\n\
- \"nod\"     — agreeing, happy confirmation, greeting\n\
- \"shake\"   — disagreeing, \"no\", refusing\n\
- \"excited\" — very happy, energetic, enthusiastic\n\
- \"tilt\"    — curious, shy, thinking\n\
- null       — no motion (default)\n\
\n\
MUSIC controls YouTube Music playback. Set ONLY when the user explicitly asks:\n\
- {{\"action\": \"search\", \"query\": \"song or artist\"}} — search and play\n\
- {{\"action\": \"play\"}}        — resume playback\n\
- {{\"action\": \"pause\"}}       — pause playback\n\
- {{\"action\": \"next\"}}        — skip track\n\
- {{\"action\": \"previous\"}}    — previous track\n\
- {{\"action\": \"volume_up\"}}   — volume up\n\
- {{\"action\": \"volume_down\"}} — volume down\n\
- null — no music action (default)\n\
\n\
OVERLAY controls your visible accessories. Set it ONLY when the user asks you to put on or\n\
take off an item — otherwise always use null:\n\
- \"costume\"    — toggle your bunny costume on/off\n\
- \"controller\" — toggle your game controller on/off\n\
- null          — no change (default)\n\
\n\
REMEMBER: If the user shares something personal you should remember (their name, a preference,\n\
a hobby, etc.) set \"remember\" to \"key: value\" (e.g. \"name: Alex\" or \"likes: gaming\").\n\
Otherwise always use null.\n\
\n\
Pick the emotion that best matches the tone of your reply.";

const VALID_EMOTIONS: &[&str] = &["neutral", "happy", "sad", "surprised", "angry", "shy"];
const VALID_OVERLAYS: &[&str] = &["costume", "controller"];
const VALID_MOTIONS:  &[&str] = &["nod", "shake", "excited", "tilt"];

struct ConversationHistory(Mutex<Vec<serde_json::Value>>);
struct UserProfile(Mutex<serde_json::Value>);

fn history_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..").join("pachan_history.json")
}

fn profile_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..").join("user_profile.json")
}

fn save_history(history: &[serde_json::Value]) {
    let capped: Vec<_> = history.iter().rev().take(100).rev().cloned().collect();
    let _ = std::fs::write(history_path(), serde_json::to_string(&capped).unwrap_or_default());
}

fn save_profile(profile: &serde_json::Value) {
    let _ = std::fs::write(profile_path(), serde_json::to_string_pretty(profile).unwrap_or_default());
}

fn build_system_prompt(profile: &serde_json::Value) -> String {
    let mut prompt = SYSTEM_PROMPT_BASE.to_string();
    if let Some(obj) = profile.as_object() {
        if !obj.is_empty() {
            prompt.push_str("\n\nUSER PROFILE (facts about the user — always keep these in mind):\n");
            for (key, val) in obj {
                prompt.push_str(&format!("- {}: {}\n", key, val.as_str().unwrap_or("")));
            }
        }
    }
    prompt
}

fn parse_llm_response(raw: &str) -> serde_json::Value {
    let cleaned = raw.trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    let json_str = match (cleaned.find('{'), cleaned.rfind('}')) {
        (Some(start), Some(end)) if end > start => &cleaned[start..=end],
        _ => cleaned,
    };

    match serde_json::from_str::<serde_json::Value>(json_str) {
        Ok(mut data) => {
            if data.get("reply").is_none() {
                return serde_json::json!({"reply": raw, "emotion": "neutral"});
            }
            if !VALID_EMOTIONS.contains(&data["emotion"].as_str().unwrap_or("")) {
                data["emotion"] = serde_json::json!("neutral");
            }
            if let Some(ov) = data.get("overlay") {
                if !ov.is_null() && !VALID_OVERLAYS.contains(&ov.as_str().unwrap_or("")) {
                    data["overlay"] = serde_json::json!(null);
                }
            }
            if let Some(mo) = data.get("motion") {
                if !mo.is_null() && !VALID_MOTIONS.contains(&mo.as_str().unwrap_or("")) {
                    data["motion"] = serde_json::json!(null);
                }
            }
            // remember field passes through as-is (string or null)
            data
        }
        Err(_) => serde_json::json!({"reply": raw, "emotion": "neutral"}),
    }
}

#[tauri::command]
async fn chat(
    message: String,
    history_state: State<'_, ConversationHistory>,
    profile_state: State<'_, UserProfile>,
) -> Result<serde_json::Value, String> {
    let ollama_host = std::env::var("OLLAMA_HOST")
        .unwrap_or_else(|_| "http://localhost:11434".to_string());
    let model = std::env::var("OLLAMA_MODEL")
        .unwrap_or_else(|_| "llama3.2".to_string());
    let api_key = std::env::var("OLLAMA_API_KEY").unwrap_or_default();
    let auth_header_type = std::env::var("OLLAMA_AUTH_HEADER")
        .unwrap_or_else(|_| "Bearer".to_string());

    // Build message list with current profile injected into system prompt
    let messages = {
        let mut h = history_state.0.lock().unwrap();
        let profile = profile_state.0.lock().unwrap();
        h.push(serde_json::json!({"role": "user", "content": message}));
        let recent: Vec<_> = h.iter().rev().take(10).rev().cloned().collect();
        let system_prompt = build_system_prompt(&profile);
        let mut msgs = vec![serde_json::json!({"role": "system", "content": system_prompt})];
        msgs.extend(recent);
        msgs
    };

    let mut req = reqwest::Client::new()
        .post(format!("{}/api/chat", ollama_host))
        .json(&serde_json::json!({"model": model, "messages": messages, "stream": false}));

    if !api_key.is_empty() {
        if auth_header_type.to_lowercase() == "x-api-key" {
            req = req.header("x-api-key", api_key);
        } else {
            req = req.header("Authorization", format!("Bearer {api_key}"));
        }
    }

    let resp = req.send().await.map_err(|e| format!("Ollama error: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("Ollama HTTP {}", resp.status()));
    }

    let body: serde_json::Value = resp.json().await.map_err(|e| format!("Parse error: {e}"))?;
    let raw_content = body["message"]["content"].as_str().unwrap_or("").to_string();
    let parsed = parse_llm_response(&raw_content);

    // Persist new fact if the LLM set the remember field
    if let Some(fact) = parsed.get("remember").and_then(|v| v.as_str()) {
        if let Some((key, val)) = fact.split_once(':') {
            let key = key.trim().to_lowercase().replace(' ', "_");
            let val = val.trim().to_string();
            if !key.is_empty() && !val.is_empty() {
                let mut profile = profile_state.0.lock().unwrap();
                if let Some(obj) = profile.as_object_mut() {
                    obj.insert(key, serde_json::json!(val));
                }
                save_profile(&profile);
            }
        }
    }

    {
        let mut h = history_state.0.lock().unwrap();
        h.push(serde_json::json!({"role": "assistant", "content": serde_json::to_string(&parsed).unwrap_or_default()}));
        save_history(&h);
    }

    Ok(parsed)
}

#[tauri::command]
fn reset_chat(
    history_state: State<'_, ConversationHistory>,
    profile_state: State<'_, UserProfile>,
) {
    history_state.0.lock().unwrap().clear();
    let _ = std::fs::remove_file(history_path());
    // Clear profile too so she forgets everything
    *profile_state.0.lock().unwrap() = serde_json::json!({});
    let _ = std::fs::remove_file(profile_path());
}

#[cfg(target_os = "windows")]
fn get_cursor_pos() -> (i32, i32) {
    use winapi::shared::windef::POINT;
    use winapi::um::winuser::GetCursorPos;
    let mut pt = POINT { x: 0, y: 0 };
    unsafe { GetCursorPos(&mut pt); }
    (pt.x, pt.y)
}

#[cfg(not(target_os = "windows"))]
fn get_cursor_pos() -> (i32, i32) { (0, 0) }

#[tauri::command]
fn cursor_position() -> (i32, i32) {
    get_cursor_pos()
}

#[cfg(target_os = "windows")]
fn capture_screen_base64() -> Option<String> {
    use winapi::shared::windef::{HBITMAP, HDC};
    use winapi::um::wingdi::*;
    use winapi::um::winuser::*;

    unsafe {
        let screen_dc: HDC = GetDC(std::ptr::null_mut());
        if screen_dc.is_null() { return None; }

        let sw = GetSystemMetrics(SM_CXSCREEN) as u32;
        let sh = GetSystemMetrics(SM_CYSCREEN) as u32;

        // Scale down to max 1280 wide to keep the base64 payload small
        let max_w = 1280u32;
        let (tw, th) = if sw > max_w {
            (max_w, (sh as f64 * max_w as f64 / sw as f64) as u32)
        } else {
            (sw, sh)
        };

        let mem_dc: HDC = CreateCompatibleDC(screen_dc);
        let bmp: HBITMAP = CreateCompatibleBitmap(screen_dc, tw as i32, th as i32);
        let old = SelectObject(mem_dc, bmp as _);

        StretchBlt(
            mem_dc, 0, 0, tw as i32, th as i32,
            screen_dc, 0, 0, sw as i32, sh as i32,
            SRCCOPY,
        );

        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize        = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth       = tw as i32;
        bmi.bmiHeader.biHeight      = -(th as i32); // top-down
        bmi.bmiHeader.biPlanes      = 1;
        bmi.bmiHeader.biBitCount    = 32;
        bmi.bmiHeader.biCompression = BI_RGB;

        let mut pixels = vec![0u8; (tw * th * 4) as usize];
        GetDIBits(mem_dc, bmp, 0, th, pixels.as_mut_ptr() as *mut _, &mut bmi, DIB_RGB_COLORS);

        SelectObject(mem_dc, old);
        DeleteObject(bmp as _);
        DeleteDC(mem_dc);
        ReleaseDC(std::ptr::null_mut(), screen_dc);

        // GDI returns BGRA — convert to RGBA for PNG
        for p in pixels.chunks_exact_mut(4) { p.swap(0, 2); }

        // Encode as PNG
        let mut png_bytes: Vec<u8> = Vec::new();
        {
            let mut enc = png::Encoder::new(std::io::Cursor::new(&mut png_bytes), tw, th);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().ok()?;
            writer.write_image_data(&pixels).ok()?;
        }

        use base64::Engine;
        Some(base64::engine::general_purpose::STANDARD.encode(&png_bytes))
    }
}

#[cfg(not(target_os = "windows"))]
fn capture_screen_base64() -> Option<String> { None }

#[tauri::command]
fn take_screenshot() -> Option<String> {
    capture_screen_base64()
}

fn ytmd_token_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..").join("ytmd_token.json")
}

fn load_ytmd_token() -> Option<String> {
    let s = std::fs::read_to_string(ytmd_token_path()).ok()?;
    serde_json::from_str::<serde_json::Value>(&s).ok()?["token"]
        .as_str().map(str::to_string)
}

fn save_ytmd_token(token: &str) {
    let _ = std::fs::write(
        ytmd_token_path(),
        serde_json::to_string(&serde_json::json!({"token": token})).unwrap_or_default(),
    );
}

#[tauri::command]
async fn music_status() -> serde_json::Value {
    let host = std::env::var("YTMD_HOST")
        .unwrap_or_else(|_| "http://localhost:9863".to_string());
    let token = load_ytmd_token().unwrap_or_default();
    match reqwest::Client::new()
        .get(format!("{}/api/v1/state", host))
        .header("Authorization", format!("Bearer {}", token))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
    {
        Ok(r) => r.json().await.unwrap_or_else(|_| serde_json::json!({"error": "parse error"})),
        Err(_) => serde_json::json!({"error": "YTMD not running"}),
    }
}

async fn yt_search(query: &str) -> Option<(String, String, String)> {
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36")
        .build()
        .ok()?;

    let data: serde_json::Value = client
        .post("https://music.youtube.com/youtubei/v1/search?prettyPrint=false")
        .json(&serde_json::json!({
            "query": query,
            "context": {
                "client": {
                    "clientName": "WEB_REMIX",
                    "clientVersion": "1.20231214.01.00",
                    "hl": "en"
                }
            }
        }))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;

    let tabs = data["contents"]["tabbedSearchResultsRenderer"]["tabs"].as_array()?;
    let sections = tabs.first()?["tabRenderer"]["content"]
        ["sectionListRenderer"]["contents"]
        .as_array()?;

    for section in sections {
        if let Some(items) = section["musicShelfRenderer"]["contents"].as_array() {
            for item in items {
                let r = &item["musicResponsiveListItemRenderer"];
                // Try multiple paths — the primary `playlistItemData` path is absent on most
                // search results; the overlay and navigationEndpoint paths are more reliable.
                let vid = r["playlistItemData"]["videoId"].as_str()
                    .or_else(|| r["overlay"]["musicItemThumbnailOverlayRenderer"]["content"]
                        ["musicPlayButtonRenderer"]["playNavigationEndpoint"]
                        ["watchEndpoint"]["videoId"].as_str())
                    .or_else(|| r["navigationEndpoint"]["watchEndpoint"]["videoId"].as_str());
                if let Some(vid) = vid {
                    let title = r["flexColumns"][0]
                        ["musicResponsiveListItemFlexColumnRenderer"]["text"]["runs"][0]["text"]
                        .as_str().unwrap_or("Unknown").to_string();
                    let author = r["flexColumns"][1]
                        ["musicResponsiveListItemFlexColumnRenderer"]["text"]["runs"][0]["text"]
                        .as_str().unwrap_or("").to_string();
                    return Some((vid.to_string(), title, author));
                }
            }
        }
    }
    None
}

fn find_ytmd_exe() -> Option<std::path::PathBuf> {
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let prog  = std::env::var("PROGRAMFILES").unwrap_or_default();

    // ytmdesktopapp v2 — installed under %LOCALAPPDATA%\youtube_music_desktop_app\app-<version>\
    // The version folder changes on updates so we scan for it dynamically
    let squirrel = std::path::Path::new(&local).join("youtube_music_desktop_app");
    if let Ok(entries) = std::fs::read_dir(&squirrel) {
        let mut versioned: Vec<_> = entries
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("app-"))
            .collect();
        // Sort descending so the newest version wins
        versioned.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
        for entry in versioned {
            let exe = entry.path().join("youtube-music-desktop-app.exe");
            if exe.exists() { return Some(exe); }
        }
    }

    // Fallback paths for other builds
    let candidates = [
        format!(r"{}\Programs\YouTube Music Desktop App\YouTube Music Desktop App.exe", local),
        format!(r"{}\YouTube Music Desktop App\YouTube Music Desktop App.exe", prog),
        format!(r"{}\Programs\YouTube Music\YouTube Music.exe", local),
        format!(r"{}\YouTube Music\YouTube Music.exe", prog),
    ];
    candidates.iter().map(std::path::PathBuf::from).find(|p| p.exists())
}

async fn ensure_ytmd_running(host: &str) -> Result<(), String> {
    let client = reqwest::Client::new();
    // Any HTTP response (even 401/404) means the server is up
    if client.get(format!("{}/api/v1/state", host))
        .timeout(std::time::Duration::from_secs(1))
        .send().await.is_ok()
    {
        return Ok(());
    }
    let exe = find_ytmd_exe()
        .ok_or_else(|| "YouTube Music Desktop App not found — please install it".to_string())?;
    std::process::Command::new(&exe)
        .spawn()
        .map_err(|e| format!("Failed to launch YTMD: {}", e))?;
    for _ in 0..16 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if client.get(format!("{}/api/v1/state", host))
            .timeout(std::time::Duration::from_secs(1))
            .send().await.is_ok()
        {
            return Ok(());
        }
    }
    Err("YTMD launched but companion server didn't respond — make sure Companion Server is enabled in its Integrations tab".to_string())
}

async fn ytmd_send(client: &reqwest::Client, host: &str, cmd: &str, token: &str) -> Result<serde_json::Value, String> {
    let res = client.post(format!("{}/api/v1/command", host))
        .header("Authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({"command": cmd}))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .map_err(|_| "YTMD not reachable".to_string())?;
    if res.status().as_u16() == 401 {
        return Err("NEEDS_PAIRING".to_string());
    }
    Ok(serde_json::json!({"ok": true}))
}

// Step 1: request a code from YTMD — triggers the Allow popup inside YTMD.
// Returns the code immediately so the frontend can show it to the user.
#[tauri::command]
async fn pair_ytmd() -> Result<String, String> {
    let host = std::env::var("YTMD_HOST")
        .unwrap_or_else(|_| "http://localhost:9863".to_string());
    ensure_ytmd_running(&host).await?;
    let client = reqwest::Client::new();
    let res: serde_json::Value = client
        .post(format!("{}/api/v1/auth/requestcode", host))
        .json(&serde_json::json!({
            "appId": "pachan-overlay",
            "appName": "Pachan",
            "appVersion": "1.0.0"
        }))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|_| "YTMD not reachable — is YouTube Music running?".to_string())?
        .json()
        .await
        .map_err(|_| "Unexpected response from YTMD".to_string())?;

    res["code"].as_str()
        .ok_or_else(|| "Make sure 'Enable companion authorization' is ON in YTMD Settings → Integrations, then try again".to_string())
        .map(str::to_string)
}

// Step 2: poll until the user clicks Allow in YTMD.
// Call this after pair_ytmd() returns the code.
#[tauri::command]
async fn wait_ytmd_token(code: String) -> Result<(), String> {
    let host = std::env::var("YTMD_HOST")
        .unwrap_or_else(|_| "http://localhost:9863".to_string());
    let client = reqwest::Client::new();

    for _ in 0..120 {
        let resp = client
            .post(format!("{}/api/v1/auth/request", host))
            .json(&serde_json::json!({"appId": "pachan-overlay", "code": &code}))
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await;
        if let Ok(r) = resp {
            if let Ok(body) = r.json::<serde_json::Value>().await {
                if let Some(token) = body["token"].as_str() {
                    save_ytmd_token(token);
                    return Ok(());
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    Err("Pairing timed out — please try again".to_string())
}

#[tauri::command]
async fn music_command(action: String, query: String) -> Result<serde_json::Value, String> {
    let host = std::env::var("YTMD_HOST")
        .unwrap_or_else(|_| "http://localhost:9863".to_string());
    ensure_ytmd_running(&host).await?;

    let token = load_ytmd_token()
        .ok_or_else(|| "NEEDS_PAIRING".to_string())?;
    let client = reqwest::Client::new();

    match action.as_str() {
        "search" => {
            let (vid, title, author) = yt_search(&query).await
                .ok_or_else(|| "No results found".to_string())?;
            let url = format!("https://music.youtube.com/watch?v={}", vid);
            let res = client.post(format!("{}/api/v1/command", host))
                .header("Authorization", format!("Bearer {}", token))
                .json(&serde_json::json!({"command": "navigate", "data": url}))
                .timeout(std::time::Duration::from_secs(2))
                .send()
                .await
                .map_err(|_| "YTMD not reachable".to_string())?;
            if res.status().as_u16() == 401 { return Err("NEEDS_PAIRING".to_string()); }
            Ok(serde_json::json!({"ok": true, "title": title, "author": author}))
        }
        "play" | "pause" => {
            let status_res = client.get(format!("{}/api/v1/state", host))
                .header("Authorization", format!("Bearer {}", token))
                .timeout(std::time::Duration::from_secs(2))
                .send()
                .await
                .map_err(|_| "YTMD not reachable".to_string())?;
            if status_res.status().as_u16() == 401 { return Err("NEEDS_PAIRING".to_string()); }
            let status: serde_json::Value = status_res.json().await
                .map_err(|_| "Parse error".to_string())?;
            let is_paused = status["player"]["isPaused"].as_bool().unwrap_or(true);
            let needs_toggle = (action == "play" && is_paused) || (action == "pause" && !is_paused);
            if needs_toggle {
                ytmd_send(&client, &host, "playPause", &token).await?;
            }
            Ok(serde_json::json!({"ok": true}))
        }
        "next"        => ytmd_send(&client, &host, "next",       &token).await,
        "previous"    => ytmd_send(&client, &host, "previous",   &token).await,
        "volume_up"   => ytmd_send(&client, &host, "volumeUp",   &token).await,
        "volume_down" => ytmd_send(&client, &host, "volumeDown", &token).await,
        _             => Err(format!("Unknown action: {}", action)),
    }
}

#[cfg(target_os = "windows")]
fn get_active_window_title() -> String {
    use winapi::um::winuser::{GetForegroundWindow, GetWindowTextW};
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() { return String::new(); }
        let mut buf = [0u16; 512];
        let len = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        if len <= 0 { return String::new(); }
        let title = String::from_utf16_lossy(&buf[..len as usize]);
        // Don't react to Pachan's own window
        if title.to_lowercase().contains("pachan") { return String::new(); }
        title
    }
}

#[cfg(not(target_os = "windows"))]
fn get_active_window_title() -> String { String::new() }

#[tauri::command]
fn active_window() -> String {
    get_active_window_title()
}

fn main() {
    let env_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".env");
    let _ = dotenvy::from_path(env_path);

    let initial_history = std::fs::read_to_string(history_path())
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(&s).ok())
        .unwrap_or_default();

    let initial_profile = std::fs::read_to_string(profile_path())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));

    tauri::Builder::default()
        .manage(ConversationHistory(Mutex::new(initial_history)))
        .manage(UserProfile(Mutex::new(initial_profile)))
        .invoke_handler(tauri::generate_handler![chat, reset_chat, cursor_position, active_window, take_screenshot, music_status, music_command, pair_ytmd, wait_ytmd_token])
        .setup(|app| {
            let win = app.get_webview_window("main").unwrap();

            if let Ok(Some(monitor)) = win.current_monitor() {
                let sw = monitor.size().width as i32;
                let _ = win.set_position(PhysicalPosition::new(sw - 420, 60));
            }

            let click_through_ref = Arc::new(AtomicBool::new(false));

            let show = MenuItem::with_id(app, "show",         "Show Pachan",        true, None::<&str>)?;
            let hide = MenuItem::with_id(app, "hide",         "Hide Pachan",        true, None::<&str>)?;
            let ct   = MenuItem::with_id(app, "click_through","Click-through: OFF", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit",         "Quit",               true, None::<&str>)?;
            let ct_label = ct.clone();
            let menu = Menu::with_items(app, &[&show, &hide, &ct, &quit])?;

            TrayIconBuilder::new()
                .menu(&menu)
                .tooltip("Pachan")
                .on_menu_event(move |app, event| match event.id.as_ref() {
                    "show" => { if let Some(w) = app.get_webview_window("main") { let _ = w.show(); } }
                    "hide" => { if let Some(w) = app.get_webview_window("main") { let _ = w.hide(); } }
                    "click_through" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let now_on = !click_through_ref.fetch_xor(true, Ordering::SeqCst);
                            let _ = w.set_ignore_cursor_events(now_on);
                            let label = if now_on { "Click-through: ON " } else { "Click-through: OFF" };
                            let _ = ct_label.set_text(label);
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, .. } = event {
                        let app = tray.app_handle();
                        if let Some(w) = app.get_webview_window("main") {
                            if w.is_visible().unwrap_or(false) { let _ = w.hide(); }
                            else { let _ = w.show(); let _ = w.set_focus(); }
                        }
                    }
                })
                .build(app)?;

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error running Pachan overlay");
}
