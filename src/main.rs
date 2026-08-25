// The Tauri + Servo shell.
//
// This process owns the OS windows (rendered by Servo via tauri-runtime-servo)
// and nothing else. The application runs in a Node sidecar this spawns, which
// asks for windows over a stdio line protocol; the renderer talks to the
// sidecar directly, however that sidecar chooses to arrange it. See README.md
// for the protocol.
//
// Nothing about the hosted app is compiled in. The frontend is read off disk
// at runtime through a registered URI scheme rather than baked into the
// binary by `tauri_build`, which is what lets one published binary serve any
// app — and what lets this repository build it, on free public-runner minutes,
// for a private repository to consume.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use tauri::utils::config::Color;
use tauri::{AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent};

type ServoRuntime = tauri_runtime_servo::Servo<tauri::EventLoopMessage>;

/// Prefix marking protocol lines on the sidecar's stdout; everything else is
/// passed through as sidecar logging.
const PREFIX: &str = "@tauri-shell ";

#[derive(Deserialize)]
#[serde(tag = "op")]
enum SidecarMessage {
    #[serde(rename = "create-window", rename_all = "camelCase")]
    CreateWindow {
        win_id: u64,
        url: String,
        width: Option<f64>,
        height: Option<f64>,
        min_width: Option<f64>,
        min_height: Option<f64>,
        title: Option<String>,
        show: Option<bool>,
        background_color: Option<String>,
    },
    #[serde(rename = "window", rename_all = "camelCase")]
    Window { win_id: u64, action: String },
}

type SharedStdin = Arc<Mutex<std::process::ChildStdin>>;

/// `#rrggbb` / `#rgb` as the sidecar sends it, to a Tauri colour. Returns None
/// for anything unparseable so a malformed value simply leaves the default.
fn parse_hex_color(value: &str) -> Option<Color> {
    let hex = value.strip_prefix('#')?;
    let (r, g, b) = match hex.len() {
        3 => {
            let digit = |i: usize| u8::from_str_radix(&hex[i..i + 1].repeat(2), 16).ok();
            (digit(0)?, digit(1)?, digit(2)?)
        }
        6 => {
            let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
            (byte(0)?, byte(2)?, byte(4)?)
        }
        _ => return None,
    };
    Some(Color(r, g, b, 255))
}

fn window_label(win_id: u64) -> String {
    format!("win-{win_id}")
}

fn send_window_event(stdin: &SharedStdin, win_id: u64, event: &str) {
    if let Ok(mut guard) = stdin.lock() {
        let line =
            format!("{{\"op\":\"window-event\",\"winId\":{win_id},\"event\":\"{event}\"}}\n");
        let _ = guard.write_all(line.as_bytes());
        let _ = guard.flush();
    }
}

fn create_window(
    handle: &AppHandle<ServoRuntime>,
    stdin: &SharedStdin,
    win_id: u64,
    url: String,
    width: Option<f64>,
    height: Option<f64>,
    min_width: Option<f64>,
    min_height: Option<f64>,
    title: Option<String>,
    show: Option<bool>,
    background_color: Option<String>,
) {
    // `<scheme>://localhost/<url>` rather than WebviewUrl::App: App resolves
    // against the store `tauri_build` embeds at compile time, which is exactly
    // the coupling this binary exists to avoid. The sidecar still sends the
    // same relative URL it always did — `index.html?winId=1&...` — so the
    // protocol is unchanged.
    let app_url: tauri::Url = match format!("{}://localhost/{url}", scheme()).parse() {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("[shell] window {win_id} has an unusable url '{url}': {error}");
            return;
        }
    };
    let mut builder = WebviewWindowBuilder::new(
        handle,
        window_label(win_id),
        WebviewUrl::CustomProtocol(app_url),
    )
    .title(title.unwrap_or_else(|| "App".to_string()))
    .inner_size(width.unwrap_or(1200.0), height.unwrap_or(800.0))
    // The sidecar mirrors Electron's hidden-then-show pattern, but an
    // unmapped GTK window has no X11 handle yet and Servo needs one to
    // create its surface — so the window is born visible.
    .visible(true);
    let _ = show;
    // Paint the native window in the app's boot theme before the webview has
    // rendered anything.
    //
    // theme-boot.js is NOT sufficient on its own, though a previous comment
    // here claimed it was. It covers the gap between first paint and app.js,
    // but not the gap before the webview's first paint at all — and in that
    // window the native surface shows its own default, which is white. Electron
    // never flashes because `backgroundColor` is set on the BrowserWindow; the
    // sidecar has always sent that value in `create-window` and this shell used
    // to drop it on the floor, since the field was not even in the struct for
    // serde to see.
    if let Some(color) = background_color.as_deref().and_then(parse_hex_color) {
        builder = builder.background_color(color);
    }
    if let (Some(w), Some(h)) = (min_width, min_height) {
        builder = builder.min_inner_size(w, h);
    }
    match builder.build() {
        Ok(window) => {
            let stdin = stdin.clone();
            window.on_window_event(move |event| match event {
                WindowEvent::CloseRequested { .. } => {
                    send_window_event(&stdin, win_id, "close-requested");
                }
                WindowEvent::Destroyed => {
                    send_window_event(&stdin, win_id, "closed");
                }
                WindowEvent::Focused(focused) => {
                    send_window_event(&stdin, win_id, if *focused { "focus" } else { "blur" });
                }
                _ => {}
            });
        }
        Err(error) => eprintln!("[shell] failed to create window {win_id}: {error}"),
    }
}

fn handle_sidecar_message(
    handle: &AppHandle<ServoRuntime>,
    stdin: &SharedStdin,
    message: SidecarMessage,
) {
    match message {
        SidecarMessage::CreateWindow {
            win_id,
            url,
            width,
            height,
            min_width,
            min_height,
            title,
            show,
            background_color,
        } => {
            let handle = handle.clone();
            let stdin = stdin.clone();
            // Window creation must happen on the main thread on macOS/Windows.
            let _ = handle.clone().run_on_main_thread(move || {
                create_window(
                    &handle,
                    &stdin,
                    win_id,
                    url,
                    width,
                    height,
                    min_width,
                    min_height,
                    title,
                    show,
                    background_color,
                );
            });
        }
        SidecarMessage::Window { win_id, action } => {
            let handle = handle.clone();
            let _ = handle.clone().run_on_main_thread(move || {
                let Some(window) = handle.get_webview_window(&window_label(win_id)) else {
                    return;
                };
                let result = match action.as_str() {
                    "show" => window.show(),
                    "hide" => window.hide(),
                    "focus" => window.set_focus(),
                    "close" => window.destroy(),
                    "maximize" => window.maximize(),
                    "minimize" => window.minimize(),
                    other => {
                        eprintln!("[shell] unknown window action '{other}'");
                        Ok(())
                    }
                };
                if let Err(error) = result {
                    eprintln!("[shell] window action '{action}' failed: {error}");
                }
            });
        }
    }
}

/// The URI scheme the frontend is served under, and therefore the page's
/// origin.
///
/// A runtime parameter because the origin is the host application's business:
/// it is what CSP `'self'` resolves to and what any origin-scoped storage is
/// keyed by, so an app that wants its own should have it. Validated rather
/// than trusted — a scheme with a `:` or a slash in it would silently produce
/// a URL that resolves somewhere else entirely.
fn scheme() -> String {
    let configured = std::env::var("TAURI_SHELL_SCHEME").unwrap_or_default();
    let configured = configured.trim();
    if configured.is_empty() {
        return "app".to_string();
    }
    let valid = configured
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
        && configured
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.');
    if valid {
        configured.to_string()
    } else {
        eprintln!("[shell] ignoring unusable TAURI_SHELL_SCHEME '{configured}'; using 'app'");
        "app".to_string()
    }
}

/// The directory the configured scheme serves.
///
/// `TAURI_SHELL_FRONTEND_DIR` when set; otherwise `dist/renderer` relative to the
/// working directory, which is the layout the sidecar's own build produces.
/// Deliberately not fatal when missing: the shell still starts, every request
/// 404s, and the log says which directory it looked in — which is a far
/// clearer failure than a blank window.
fn frontend_dir() -> PathBuf {
    if let Ok(explicit) = std::env::var("TAURI_SHELL_FRONTEND_DIR") {
        return PathBuf::from(explicit);
    }
    PathBuf::from("dist/renderer")
}

fn content_type_for(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext) {
        Some("html") => "text/html",
        Some("js" | "mjs") => "text/javascript",
        Some("css") => "text/css",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("wasm") => "application/wasm",
        Some("map") => "application/json",
        _ => "application/octet-stream",
    }
}

/// The path a `<scheme>://localhost/<path>?<query>` request asks for, rejected if
/// it tries to climb out of the frontend directory.
///
/// The scheme is same-origin to the page, so anything the page can reach can
/// name a path here; `..` is refused rather than normalised so a traversal is
/// a 404 in the log rather than a silent read of an arbitrary file.
fn request_path(uri: &str) -> Option<String> {
    let after_scheme = uri.split_once("://")?.1;
    let path = after_scheme.split_once('/').map_or("", |(_, rest)| rest);
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let path = if path.is_empty() { "index.html" } else { path };
    if path
        .split('/')
        .any(|segment| segment == ".." || segment == ".")
    {
        return None;
    }
    Some(path.to_string())
}

/// Locate `dist/sidecar/index.js`.
///
/// `TAURI_SHELL_SIDECAR_ENTRY` wins when set. Otherwise: the historical default is
/// `../dist/sidecar/index.js`, which is *cwd*-relative and so only resolves
/// when the shell is started from inside `tauri-shell/` — which `cargo run`
/// does and a perf harness invoking the release binary by path does not. The
/// failure is a bare Node MODULE_NOT_FOUND naming a path nobody wrote, so try
/// the exe-relative location too and, when neither exists, say what was tried.
fn sidecar_entry() -> std::io::Result<PathBuf> {
    if let Ok(explicit) = std::env::var("TAURI_SHELL_SIDECAR_ENTRY") {
        return Ok(PathBuf::from(explicit));
    }
    let mut candidates = vec![PathBuf::from("../dist/sidecar/index.js")];
    // target/release/tauri-shell → up three to the repo root.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(root) = exe
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
        {
            candidates.push(root.join("../dist/sidecar/index.js"));
        }
    }
    for candidate in &candidates {
        if candidate.exists() {
            return Ok(candidate.clone());
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!(
            "no dist/sidecar/index.js (tried {}) — run `pnpm build && pnpm build:tauri`, \
             or point TAURI_SHELL_SIDECAR_ENTRY at it",
            candidates
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    ))
}

/// How long to wait for the sidecar's first `create-window` before assuming
/// the two ends disagree.
///
/// The failure this catches is silent by construction: the sidecar decides
/// whether a shell is attached by looking for `TAURI_SHELL=1`, and reads
/// protocol lines by their prefix. Get either wrong — an old sidecar, a
/// renamed variable — and it simply never asks for a window. Nothing errors;
/// there is just no application. Worth a line in the log rather than a
/// mystery.
const FIRST_WINDOW_GRACE: std::time::Duration = std::time::Duration::from_secs(15);

fn spawn_sidecar(handle: AppHandle<ServoRuntime>, alive: Arc<AtomicBool>) -> std::io::Result<()> {
    let node = std::env::var("TAURI_SHELL_SIDECAR_NODE").unwrap_or_else(|_| "node".to_string());
    let entry = sidecar_entry()?;
    let mut child = Command::new(node)
        .arg(entry)
        .env("TAURI_SHELL", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;

    let stdin: SharedStdin = Arc::new(Mutex::new(
        child.stdin.take().expect("sidecar stdin is piped"),
    ));
    let stdout = child.stdout.take().expect("sidecar stdout is piped");

    let asked_for_a_window = Arc::new(AtomicBool::new(false));
    let watchdog = asked_for_a_window.clone();
    std::thread::spawn(move || {
        std::thread::sleep(FIRST_WINDOW_GRACE);
        if !watchdog.load(Ordering::SeqCst) {
            eprintln!(
                "[shell] the sidecar has not asked for a window in {}s. It may not \
                 have recognised this shell: it looks for TAURI_SHELL=1 in its \
                 environment and for protocol lines prefixed '{PREFIX}'.",
                FIRST_WINDOW_GRACE.as_secs()
            );
        }
    });

    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            let Some(payload) = line.strip_prefix(PREFIX) else {
                println!("[sidecar] {line}");
                continue;
            };
            match serde_json::from_str::<SidecarMessage>(payload) {
                Ok(message) => {
                    if matches!(message, SidecarMessage::CreateWindow { .. }) {
                        asked_for_a_window.store(true, Ordering::SeqCst);
                    }
                    handle_sidecar_message(&handle, &stdin, message)
                }
                Err(error) => eprintln!("[shell] bad sidecar message: {error}: {payload}"),
            }
        }
        // Sidecar stdout closed: the app process is gone; take the shell down.
        eprintln!("[shell] sidecar exited; shutting down");
        alive.store(false, Ordering::SeqCst);
        let _ = child.wait();
        handle.exit(0);
    });

    Ok(())
}

fn main() {
    let sidecar_alive = Arc::new(AtomicBool::new(true));
    let alive_for_setup = sidecar_alive.clone();

    let root = frontend_dir();
    let scheme = scheme();
    println!(
        "[shell] serving {scheme}://localhost/ from {}",
        root.display()
    );

    let app = tauri::Builder::<ServoRuntime>::new()
        // Servo cannot read custom protocol request bodies; route Tauri's own
        // internal invokes through the postMessage bridge (the app's IPC does
        // not use Tauri invokes at all — it rides the sidecar WebSocket).
        .invoke_system(tauri_runtime_servo::INVOKE_SYSTEM_SCRIPT)
        // The frontend, off disk. Registered rather than embedded — see the
        // note on this file. A registered custom scheme gets a tuple origin
        // and counts as potentially trustworthy on the patched engine, so the
        // page keeps CSP 'self' and the secure-context APIs it would have had
        // on tauri://localhost.
        .register_uri_scheme_protocol(scheme, move |_ctx, request| {
            let uri = request.uri().to_string();
            let Some(path) = request_path(&uri) else {
                eprintln!("[shell] refused traversal in {uri}");
                return tauri::http::Response::builder()
                    .status(403)
                    .body(Vec::new())
                    .expect("static response builds");
            };
            match fs::read(root.join(&path)) {
                Ok(body) => tauri::http::Response::builder()
                    .header("Content-Type", content_type_for(&path))
                    .body(body)
                    .expect("static response builds"),
                Err(error) => {
                    eprintln!("[shell] 404 {path}: {error}");
                    tauri::http::Response::builder()
                        .status(404)
                        .body(Vec::new())
                        .expect("static response builds")
                }
            }
        })
        .setup(move |app| {
            spawn_sidecar(app.handle().clone(), alive_for_setup)?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(move |_handle, event| {
        if let RunEvent::ExitRequested { api, .. } = event {
            // No windows exist until the sidecar asks for one, and windows may
            // all close while the sidecar keeps working; only a dead sidecar
            // ends the shell.
            if sidecar_alive.load(Ordering::SeqCst) {
                api.prevent_exit();
            }
        }
    });
}
