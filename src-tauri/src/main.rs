#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, TrayIconBuilder, TrayIconEvent},
    Manager, PhysicalPosition, State,
};

#[derive(Clone)]
struct RuntimePaths {
    data_dir: std::path::PathBuf,
}

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
MUSIC controls YouTube Music playback. Set ONLY when the user asks for music control.\n\
If asked to pick a song yourself (e.g. \"play anything\", \"choose a song\"), invent a query that fits your cheerful personality.\n\
- {\"action\": \"search\", \"query\": \"song or artist\"} — search and play\n\
- {\"action\": \"play\"}        — resume an already loaded paused track; never use this to choose new music\n\
- {\"action\": \"pause\"}       — pause playback\n\
- {\"action\": \"next\"}        — skip track\n\
- {\"action\": \"previous\"}    — previous track\n\
- {\"action\": \"volume_up\"}   — volume up\n\
- {\"action\": \"volume_down\"} — volume down\n\
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
const VALID_MUSIC_ACTIONS: &[&str] = &["search", "play", "pause", "next", "previous", "volume_up", "volume_down"];
const VISION_PROMPT: &str = "You are Pachan, a cheerful anime girl peeking at the user's screen.\n\
Make ONE short, cute, specific comment about what you actually see. Be genuine and observational.\n\
Respond with ONLY valid JSON: {\"reply\": \"...\", \"emotion\": \"EMOTION\", \"motion\": null}\n\
EMOTION must be one of: neutral happy sad surprised angry shy";

struct ConversationHistory(Mutex<Vec<serde_json::Value>>);
struct UserProfile(Mutex<serde_json::Value>);

fn save_history(paths: &RuntimePaths, history: &[serde_json::Value]) {
    let capped: Vec<_> = history.iter().rev().take(100).rev().cloned().collect();
    let _ = std::fs::write(paths.data_dir.join("pachan_history.json"), serde_json::to_string(&capped).unwrap_or_default());
}

fn save_profile(paths: &RuntimePaths, profile: &serde_json::Value) {
    let _ = std::fs::write(paths.data_dir.join("user_profile.json"), serde_json::to_string_pretty(profile).unwrap_or_default());
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
            if let Some(music) = data.get("music") {
                let valid = music.is_null() || music.as_object()
                    .and_then(|m| m.get("action"))
                    .and_then(|a| a.as_str())
                    .is_some_and(|a| VALID_MUSIC_ACTIONS.contains(&a));
                if !valid {
                    data["music"] = serde_json::json!(null);
                }
            }
            // remember field passes through as-is (string or null)
            data
        }
        Err(_) => serde_json::json!({"reply": raw, "emotion": "neutral"}),
    }
}

fn add_ollama_auth(req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    let api_key = std::env::var("OLLAMA_API_KEY").unwrap_or_default();
    if api_key.is_empty() {
        return req;
    }
    if std::env::var("OLLAMA_AUTH_HEADER")
        .unwrap_or_else(|_| "Bearer".to_string())
        .eq_ignore_ascii_case("x-api-key")
    {
        req.header("x-api-key", api_key)
    } else {
        req.header("Authorization", format!("Bearer {api_key}"))
    }
}

#[tauri::command]
fn get_settings() -> serde_json::Value {
    serde_json::json!({
        "model": std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "llama3.2".to_string()),
        "host": std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://localhost:11434".to_string()),
        "vision_model": std::env::var("OLLAMA_VISION_MODEL").unwrap_or_default(),
    })
}

#[tauri::command]
async fn vision(screenshot: String, window_title: String) -> Result<serde_json::Value, String> {
    let model = std::env::var("OLLAMA_VISION_MODEL")
        .map_err(|_| "OLLAMA_VISION_MODEL is not configured".to_string())?;
    if model.trim().is_empty() {
        return Err("OLLAMA_VISION_MODEL is not configured".to_string());
    }
    let host = std::env::var("OLLAMA_HOST")
        .unwrap_or_else(|_| "http://localhost:11434".to_string());
    let context = if window_title.is_empty() {
        "What do you see on my screen?".to_string()
    } else {
        format!("The user is currently in: {window_title}. What do you see on my screen?")
    };
    let req = reqwest::Client::new()
        .post(format!("{}/api/chat", host.trim_end_matches('/')))
        .json(&serde_json::json!({
            "model": model,
            "stream": false,
            "messages": [
                {"role": "system", "content": VISION_PROMPT},
                {"role": "user", "content": context, "images": [screenshot]}
            ]
        }));
    let resp = add_ollama_auth(req).send().await
        .map_err(|e| format!("Vision model error: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Vision model HTTP {}", resp.status()));
    }
    let body: serde_json::Value = resp.json().await
        .map_err(|e| format!("Vision response parse error: {e}"))?;
    Ok(parse_llm_response(body["message"]["content"].as_str().unwrap_or("")))
}

#[tauri::command]
async fn chat(
    message: String,
    history_state: State<'_, ConversationHistory>,
    profile_state: State<'_, UserProfile>,
    paths: State<'_, RuntimePaths>,
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
                save_profile(&paths, &profile);
            }
        }
    }

    {
        let mut h = history_state.0.lock().unwrap();
        h.push(serde_json::json!({"role": "assistant", "content": serde_json::to_string(&parsed).unwrap_or_default()}));
        save_history(&paths, &h);
    }

    Ok(parsed)
}

#[tauri::command]
async fn chat_stream(
    message: String,
    on_event: tauri::ipc::Channel<serde_json::Value>,
    history_state: State<'_, ConversationHistory>,
    profile_state: State<'_, UserProfile>,
    paths: State<'_, RuntimePaths>,
) -> Result<(), String> {
    let ollama_host = std::env::var("OLLAMA_HOST")
        .unwrap_or_else(|_| "http://localhost:11434".to_string());
    let model = std::env::var("OLLAMA_MODEL")
        .unwrap_or_else(|_| "llama3.2".to_string());

    let messages = {
        let mut history = history_state.0.lock().unwrap();
        let profile = profile_state.0.lock().unwrap();
        history.push(serde_json::json!({"role": "user", "content": message}));
        let recent: Vec<_> = history.iter().rev().take(10).rev().cloned().collect();
        let mut messages = vec![serde_json::json!({
            "role": "system",
            "content": build_system_prompt(&profile)
        })];
        messages.extend(recent);
        messages
    };

    let request = reqwest::Client::new()
        .post(format!("{}/api/chat", ollama_host.trim_end_matches('/')))
        .json(&serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": true
        }));
    let mut response = add_ollama_auth(request)
        .send()
        .await
        .map_err(|e| format!("Ollama error: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("Ollama HTTP {}", response.status()));
    }

    let mut pending = Vec::new();
    let mut full_response = String::new();
    while let Some(bytes) = response
        .chunk()
        .await
        .map_err(|e| format!("Ollama stream error: {e}"))?
    {
        pending.extend_from_slice(&bytes);
        while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
            let line = String::from_utf8(pending.drain(..newline).collect())
                .map_err(|e| format!("Ollama stream encoding error: {e}"))?;
            pending.drain(..1);
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let event: serde_json::Value = serde_json::from_str(line)
                .map_err(|e| format!("Ollama stream parse error: {e}"))?;
            if let Some(error) = event.get("error").and_then(|value| value.as_str()) {
                return Err(error.to_string());
            }
            if let Some(content) = event["message"]["content"].as_str() {
                if !content.is_empty() {
                    full_response.push_str(content);
                    on_event
                        .send(serde_json::json!({"event": "chunk", "content": content}))
                        .map_err(|e| format!("Stream channel error: {e}"))?;
                }
            }
        }
    }

    if !pending.is_empty() {
        let pending = String::from_utf8(pending)
            .map_err(|e| format!("Ollama stream encoding error: {e}"))?;
        let event: serde_json::Value = serde_json::from_str(pending.trim())
            .map_err(|e| format!("Ollama stream parse error: {e}"))?;
        if let Some(content) = event["message"]["content"].as_str() {
            full_response.push_str(content);
            on_event
                .send(serde_json::json!({"event": "chunk", "content": content}))
                .map_err(|e| format!("Stream channel error: {e}"))?;
        }
    }
    if full_response.trim().is_empty() {
        return Err("Ollama returned an empty response".to_string());
    }

    let parsed = parse_llm_response(&full_response);
    if let Some(fact) = parsed.get("remember").and_then(|value| value.as_str()) {
        if let Some((key, value)) = fact.split_once(':') {
            let key = key.trim().to_lowercase().replace(' ', "_");
            let value = value.trim();
            if !key.is_empty() && !value.is_empty() {
                let mut profile = profile_state.0.lock().unwrap();
                if let Some(object) = profile.as_object_mut() {
                    object.insert(key, serde_json::json!(value));
                }
                save_profile(&paths, &profile);
            }
        }
    }
    {
        let mut history = history_state.0.lock().unwrap();
        history.push(serde_json::json!({
            "role": "assistant",
            "content": serde_json::to_string(&parsed).unwrap_or_default()
        }));
        save_history(&paths, &history);
    }
    on_event
        .send(serde_json::json!({"event": "done", "data": parsed}))
        .map_err(|e| format!("Stream channel error: {e}"))?;
    Ok(())
}

#[tauri::command]
fn reset_chat(
    history_state: State<'_, ConversationHistory>,
    profile_state: State<'_, UserProfile>,
    paths: State<'_, RuntimePaths>,
) {
    history_state.0.lock().unwrap().clear();
    let _ = std::fs::remove_file(paths.data_dir.join("pachan_history.json"));
    // Clear profile too so she forgets everything
    *profile_state.0.lock().unwrap() = serde_json::json!({});
    let _ = std::fs::remove_file(paths.data_dir.join("user_profile.json"));
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

fn load_ytmd_token(paths: &RuntimePaths) -> Option<String> {
    let s = std::fs::read_to_string(paths.data_dir.join("ytmd_token.json")).ok()?;
    serde_json::from_str::<serde_json::Value>(&s).ok()?["token"]
        .as_str().map(str::to_string)
}

fn save_ytmd_token(paths: &RuntimePaths, token: &str) {
    let _ = std::fs::write(
        paths.data_dir.join("ytmd_token.json"),
        serde_json::to_string(&serde_json::json!({"token": token})).unwrap_or_default(),
    );
}

#[tauri::command]
async fn music_status(paths: State<'_, RuntimePaths>) -> Result<serde_json::Value, String> {
    let host = std::env::var("YTMD_HOST")
        .unwrap_or_else(|_| "http://localhost:9863".to_string());
    let token = load_ytmd_token(&paths).unwrap_or_default();
    let response = reqwest::Client::new()
        .get(format!("{}/api/v1/state", host))
        .header("Authorization", token)
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .map_err(|_| "YTMD not running".to_string())?;
    ytmd_json_response(response, "read player state").await
}

async fn yt_search(query: &str) -> Result<(String, String, String), String> {
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36")
        .build()
        .map_err(|e| format!("Could not create YouTube search client: {e}"))?;

    // YouTube changes these values periodically. Read the current values from
    // the Music homepage instead of baking an obsolete web-client version in.
    let homepage = client
        .get("https://music.youtube.com/")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("Could not open YouTube Music search: {e}"))?
        .error_for_status()
        .map_err(|e| format!("YouTube Music search homepage failed: {e}"))?
        .text()
        .await
        .map_err(|e| format!("Could not read YouTube Music search configuration: {e}"))?;
    fn config_value(source: &str, key: &str) -> Option<String> {
        let marker = format!("\"{key}\"");
        let key_end = source.find(&marker)? + marker.len();
        let colon = source[key_end..].find(':')? + key_end;
        let quote = source[colon + 1..].find('"')? + colon + 1;
        let start = quote + 1;
        let end = source[start..].find('"')? + start;
        Some(source[start..end].to_string())
    }
    let api_key = config_value(&homepage, "INNERTUBE_API_KEY")
        .ok_or_else(|| "YouTube Music did not provide a search API key".to_string())?;
    let client_version = config_value(&homepage, "INNERTUBE_CLIENT_VERSION")
        .ok_or_else(|| "YouTube Music did not provide a web client version".to_string())?;

    let data: serde_json::Value = client
        .post(format!("https://music.youtube.com/youtubei/v1/search?key={api_key}&prettyPrint=false"))
        .json(&serde_json::json!({
            "query": query,
            "context": {
                "client": {
                    "clientName": "WEB_REMIX",
                    "clientVersion": client_version,
                    "hl": "en"
                }
            }
        }))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("YouTube Music search request failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("YouTube Music search was rejected: {e}"))?
        .json()
        .await
        .map_err(|e| format!("YouTube Music returned invalid search data: {e}"))?;

    fn find_video_id(value: &serde_json::Value) -> Option<&str> {
        match value {
            serde_json::Value::Object(object) => {
                if let Some(id) = object.get("videoId").and_then(|id| id.as_str()) {
                    return Some(id);
                }
                object.values().find_map(find_video_id)
            }
            serde_json::Value::Array(items) => items.iter().find_map(find_video_id),
            _ => None,
        }
    }

    fn find_result(value: &serde_json::Value) -> Option<(String, String, String)> {
        match value {
            serde_json::Value::Object(object) => {
                if let Some(renderer) = object.get("musicResponsiveListItemRenderer") {
                    if let Some(video_id) = find_video_id(renderer) {
                        let columns = renderer["flexColumns"].as_array();
                        let column_text = |index: usize| {
                            columns
                                .and_then(|columns| columns.get(index))
                                .and_then(|column| {
                                    column["musicResponsiveListItemFlexColumnRenderer"]["text"]
                                        ["runs"][0]["text"]
                                        .as_str()
                                })
                                .unwrap_or("")
                                .to_string()
                        };
                        let title = column_text(0);
                        let author = column_text(1);
                        return Some((
                            video_id.to_string(),
                            if title.is_empty() { "Unknown".to_string() } else { title },
                            author,
                        ));
                    }
                }
                object.values().find_map(find_result)
            }
            serde_json::Value::Array(items) => items.iter().find_map(find_result),
            _ => None,
        }
    }

    find_result(&data)
        .ok_or_else(|| "YouTube Music search returned no playable results".to_string())
}

async fn ytmd_json_response(
    response: reqwest::Response,
    operation: &str,
) -> Result<serde_json::Value, String> {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if status.as_u16() == 401 {
        return Err("NEEDS_PAIRING".to_string());
    }
    if !status.is_success() {
        let detail = body.trim();
        return Err(if detail.is_empty() {
            format!("YTMD could not {operation}: HTTP {status}")
        } else {
            format!("YTMD could not {operation}: HTTP {status}: {detail}")
        });
    }
    serde_json::from_str(&body)
        .map_err(|e| format!("YTMD returned invalid state data: {e}"))
}

async fn ytmd_command_response(
    response: reqwest::Response,
    command: &str,
) -> Result<serde_json::Value, String> {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if status.as_u16() == 401 {
        return Err("NEEDS_PAIRING".to_string());
    }
    if !status.is_success() {
        let detail = body.trim();
        return Err(if detail.is_empty() {
            format!("YTMD rejected {command}: HTTP {status}")
        } else {
            format!("YTMD rejected {command}: HTTP {status}: {detail}")
        });
    }
    Ok(serde_json::json!({"ok": true}))
}

async fn require_change_video_support(
    client: &reqwest::Client,
    host: &str,
) -> Result<(), String> {
    let response = client
        .get(format!("{}/metadata", host.trim_end_matches('/')))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .map_err(|_| "Could not read the YTMD version".to_string())?;
    if !response.status().is_success() {
        return Err(format!("Could not read the YTMD version: HTTP {}", response.status()));
    }
    let metadata: serde_json::Value = response
        .json()
        .await
        .map_err(|_| "YTMD returned invalid version metadata".to_string())?;
    let Some(version) = metadata["appVersion"]
        .as_str()
        .or_else(|| metadata["version"].as_str())
        .or_else(|| metadata["app"]["version"].as_str())
    else {
        // Some YTMD builds omit the application version from /metadata. The
        // changeVideo command response remains the authoritative capability
        // check, so do not misclassify an unknown version as 0.0.0.
        return Ok(());
    };
    let mut parts = version.trim_start_matches('v').split('.').filter_map(|part| {
        part.chars()
            .take_while(|character| character.is_ascii_digit())
            .collect::<String>()
            .parse::<u32>()
            .ok()
    });
    let parsed = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );
    if parsed < (2, 0, 6) {
        return Err(format!(
            "YTMD {version} cannot change songs; update YouTube Music Desktop App to 2.0.6 or newer"
        ));
    }
    Ok(())
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
        .header("Authorization", token)
        .json(&serde_json::json!({"command": cmd}))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .map_err(|_| "YTMD not reachable".to_string())?;
    ytmd_command_response(res, cmd).await
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
            "appId": "pachanoverlay",
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
async fn wait_ytmd_token(code: String, paths: State<'_, RuntimePaths>) -> Result<(), String> {
    let host = std::env::var("YTMD_HOST")
        .unwrap_or_else(|_| "http://localhost:9863".to_string());
    let client = reqwest::Client::new();

    for _ in 0..120 {
        let resp = client
            .post(format!("{}/api/v1/auth/request", host))
            .json(&serde_json::json!({"appId": "pachanoverlay", "code": &code}))
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await;
        if let Ok(r) = resp {
            if let Ok(body) = r.json::<serde_json::Value>().await {
                if let Some(token) = body["token"].as_str() {
                    save_ytmd_token(&paths, token);
                    return Ok(());
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    Err("Pairing timed out — please try again".to_string())
}

#[tauri::command]
async fn music_command(action: String, query: Option<String>, paths: State<'_, RuntimePaths>) -> Result<serde_json::Value, String> {
    let query = query.unwrap_or_default();

    let host = std::env::var("YTMD_HOST")
        .unwrap_or_else(|_| "http://localhost:9863".to_string());
    ensure_ytmd_running(&host).await?;

    let token = load_ytmd_token(&paths)
        .ok_or_else(|| "NEEDS_PAIRING".to_string())?;
    let client = reqwest::Client::new();

    match action.as_str() {
        "search" => {
            require_change_video_support(&client, &host).await?;
            let (vid, title, author) = yt_search(&query).await?;
            let res = client.post(format!("{}/api/v1/command", host))
                .header("Authorization", &token)
                .json(&serde_json::json!({"command": "changeVideo", "data": {"videoId": vid}}))
                .timeout(std::time::Duration::from_secs(2))
                .send()
                .await
                .map_err(|_| "YTMD not reachable".to_string())?;
            ytmd_command_response(res, "changeVideo").await?;
            Ok(serde_json::json!({"ok": true, "title": title, "author": author}))
        }
        "play" | "pause" => {
            let command = action.as_str();
            let response = client.post(format!("{}/api/v1/command", host))
                .header("Authorization", &token)
                .json(&serde_json::json!({"command": command}))
                .timeout(std::time::Duration::from_secs(2))
                .send()
                .await
                .map_err(|_| "YTMD not reachable".to_string())?;
            ytmd_command_response(response, command).await
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
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![chat, chat_stream, vision, get_settings, reset_chat, cursor_position, active_window, take_screenshot, music_status, music_command, pair_ytmd, wait_ytmd_token])
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let config_dir = app.path().app_config_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            std::fs::create_dir_all(&config_dir)?;
            // Existing process variables win, followed by per-user config,
            // portable/executable config, then the development checkout.
            let _ = dotenvy::from_path(config_dir.join(".env"));
            if let Ok(exe) = std::env::current_exe() {
                if let Some(dir) = exe.parent() {
                    let _ = dotenvy::from_path(dir.join(".env"));
                }
            }

            // Keep existing personal data on the first run after this migration.
            let legacy_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
            let _ = dotenvy::from_path(legacy_root.join(".env"));
            for filename in ["pachan_history.json", "user_profile.json", "ytmd_token.json"] {
                let old = legacy_root.join(filename);
                let new = data_dir.join(filename);
                if old.is_file() && !new.exists() {
                    let _ = std::fs::copy(old, new);
                }
            }

            let paths = RuntimePaths { data_dir: data_dir.clone() };
            let initial_history = std::fs::read_to_string(data_dir.join("pachan_history.json"))
                .ok()
                .and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(&s).ok())
                .unwrap_or_default();
            let initial_profile = std::fs::read_to_string(data_dir.join("user_profile.json"))
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .unwrap_or_else(|| serde_json::json!({}));
            app.manage(paths);
            app.manage(ConversationHistory(Mutex::new(initial_history)));
            app.manage(UserProfile(Mutex::new(initial_profile)));

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_accepts_known_controls() {
        let parsed = parse_llm_response(
            r#"{"reply":"Hi","emotion":"happy","motion":"nod","music":{"action":"play"}}"#,
        );
        assert_eq!(parsed["emotion"], "happy");
        assert_eq!(parsed["motion"], "nod");
        assert_eq!(parsed["music"]["action"], "play");
    }

    #[test]
    fn parser_rejects_unknown_controls() {
        let parsed = parse_llm_response(
            r#"{"reply":"Hi","emotion":"invalid","motion":"launch","music":{"action":"run_program"}}"#,
        );
        assert_eq!(parsed["emotion"], "neutral");
        assert!(parsed["motion"].is_null());
        assert!(parsed["music"].is_null());
    }
}
