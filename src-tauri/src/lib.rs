mod audio;
mod engine;
mod models;
mod paste;
mod settings;

use engine::{Engine, EngineState, EngineStatus};
use models::ModelSpec;
use serde::Serialize;
use settings::{Settings, ShortcutMode};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, RunEvent, WindowEvent};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

/// Une dictée en cours : capture + état de la transcription incrémentale.
struct Session {
    id: u64,
    recorder: audio::Recorder,
    live: Mutex<LiveState>,
    /// Dictée directe : un champ texte était actif au démarrage, les segments
    /// validés y sont insérés au fil de la parole.
    direct: bool,
}

#[derive(Default, Clone)]
struct LiveState {
    /// Texte des segments déjà validés (coupés à une pause).
    committed: String,
    /// Index (échantillons) du début du segment en cours.
    boundary: usize,
    /// Dernière transcription provisoire du segment en cours.
    partial: String,
    /// Texte provisoire actuellement tapé dans le champ actif (mode direct).
    typed_partial: String,
}

pub struct AppState {
    data_dir: PathBuf,
    settings: Mutex<Settings>,
    engine: tokio::sync::Mutex<Option<Arc<dyn Engine>>>,
    session: Mutex<Option<Arc<Session>>>,
    live_task: tokio::sync::Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    downloading: Mutex<HashSet<String>>,
    last_press: Mutex<Option<Instant>>,
    shortcut_error: Mutex<Option<String>>,
    overlay_gen: AtomicU64,
    session_seq: AtomicU64,
}

impl AppState {
    fn settings_path(&self) -> PathBuf {
        self.data_dir.join("settings.json")
    }
    fn models_root(&self) -> PathBuf {
        self.data_dir.join("models")
    }
    fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }
    fn is_recording(&self) -> bool {
        self.session.lock().unwrap().is_some()
    }
    fn session_active(&self, id: u64) -> bool {
        self.session.lock().unwrap().as_ref().map(|s| s.id == id).unwrap_or(false)
    }
}

#[derive(Serialize, Clone)]
struct ModelInfo {
    #[serde(flatten)]
    spec: ModelSpec,
    downloaded: bool,
    downloading: bool,
}

#[derive(Serialize, Clone)]
struct RuntimeInfo {
    kind: engine::EngineKind,
    id: &'static str,
    label: &'static str,
    binary: &'static str,
    found: Option<String>,
    install_hint: &'static str,
}

#[derive(Serialize, Clone)]
struct Snapshot {
    settings: Settings,
    models: Vec<ModelInfo>,
    engine: EngineStatus,
    runtimes: Vec<RuntimeInfo>,
    accessibility: bool,
    shortcut_error: Option<String>,
    recording: bool,
    models_dir: String,
    platform: &'static str,
}

async fn snapshot(state: &AppState) -> Snapshot {
    let settings = state.settings();
    let downloading = state.downloading.lock().unwrap().clone();
    let root = state.models_root();
    let models = models::catalog()
        .into_iter()
        .map(|spec| {
            let dir = models::model_dir(&root, &spec.id);
            ModelInfo {
                downloaded: models::is_downloaded(&spec, &dir),
                downloading: downloading.contains(&spec.id),
                spec,
            }
        })
        .collect();
    let engine = match state.engine.lock().await.as_ref() {
        Some(e) => e.status().await,
        None => EngineStatus::stopped(),
    };
    let runtimes = engine::EngineKind::all()
        .iter()
        .map(|k| RuntimeInfo {
            kind: *k,
            id: k.id(),
            label: k.label(),
            binary: k.binary(),
            found: engine::find_binary(*k, settings.runtime_override(*k), Some(&state.data_dir)).map(|p| p.to_string_lossy().to_string()),
            install_hint: k.install_hint(),
        })
        .collect();
    Snapshot {
        runtimes,
        accessibility: paste::accessibility_trusted(),
        shortcut_error: state.shortcut_error.lock().unwrap().clone(),
        settings,
        models,
        engine,
        recording: state.is_recording(),
        models_dir: root.to_string_lossy().to_string(),
        platform: std::env::consts::OS,
    }
}

fn notify_changed(app: &AppHandle) {
    let _ = app.emit("state-changed", ());
}

// ---------------------------------------------------------------------------
// Overlay
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
struct OverlayMsg {
    phase: String,
    text: String,
    hint: String,
}

#[derive(Serialize, Clone)]
struct LiveMsg {
    committed: String,
    partial: String,
}

fn overlay_show(app: &AppHandle, phase: &str, text: &str, hint: &str) {
    let state = app.state::<AppState>();
    state.overlay_gen.fetch_add(1, Ordering::SeqCst);
    if let Some(win) = app.get_webview_window("overlay") {
        // Bas de l'écran où se trouve le curseur.
        let monitor = app
            .cursor_position()
            .ok()
            .and_then(|p| app.monitor_from_point(p.x, p.y).ok().flatten())
            .or_else(|| app.primary_monitor().ok().flatten());
        if let (Some(mon), Ok(size)) = (monitor, win.outer_size()) {
            let ms = mon.size();
            let mp = mon.position();
            let margin = (64.0 * mon.scale_factor()) as i32;
            let x = mp.x + (ms.width as i32 - size.width as i32) / 2;
            let y = mp.y + ms.height as i32 - size.height as i32 - margin;
            let _ = win.set_position(PhysicalPosition::new(x, y));
        }
        let _ = win.emit(
            "overlay",
            OverlayMsg {
                phase: phase.into(),
                text: text.into(),
                hint: hint.into(),
            },
        );
        let _ = win.show();
    }
}

fn overlay_live(app: &AppHandle, committed: &str, partial: &str) {
    if let Some(win) = app.get_webview_window("overlay") {
        let _ = win.emit(
            "live",
            LiveMsg {
                committed: committed.into(),
                partial: partial.into(),
            },
        );
    }
}

fn overlay_hide_after(app: &AppHandle, ms: u64) {
    let state = app.state::<AppState>();
    let gen = state.overlay_gen.load(Ordering::SeqCst);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(ms)).await;
        let state = app.state::<AppState>();
        if state.overlay_gen.load(Ordering::SeqCst) == gen {
            if let Some(win) = app.get_webview_window("overlay") {
                let _ = win.hide();
            }
        }
    });
}

fn overlay_error(app: &AppHandle, msg: &str) {
    overlay_show(app, "error", msg, "");
    overlay_hide_after(app, 2600);
}

// ---------------------------------------------------------------------------
// Moteur
// ---------------------------------------------------------------------------

async fn start_engine(app: AppHandle) {
    let state = app.state::<AppState>();
    let settings = state.settings();
    let Some(spec) = models::find(&settings.model_id) else {
        return;
    };
    let dir = models::model_dir(&state.models_root(), &spec.id);
    {
        let mut cur = state.engine.lock().await;
        if let Some(old) = cur.take() {
            old.stop().await;
        }
    }
    if !models::is_downloaded(&spec, &dir) {
        notify_changed(&app);
        return;
    }
    if spec.engine == engine::EngineKind::SherpaOnnx
        && engine::find_binary(spec.engine, settings.runtime_override(spec.engine), Some(&state.data_dir)).is_none()
    {
        let app2 = app.clone();
        let res = engine::sherpa::ensure_runtime(&state.data_dir, move |got, total| {
            let _ = app2.emit("runtime-progress", serde_json::json!({ "kind": "sherpa-onnx", "downloaded": got, "total": total }));
        })
        .await;
        let _ = app.emit("runtime-progress", serde_json::json!({ "kind": "sherpa-onnx", "done": true }));
        if let Err(e) = res {
            log::error!("installation du runtime sherpa-onnx : {e}");
        }
    }
    let eng = engine::for_kind(spec.engine, &state.data_dir);
    {
        let mut cur = state.engine.lock().await;
        *cur = Some(eng.clone());
    }
    notify_changed(&app);
    if let Err(e) = eng.start(&spec, &dir, settings.runtime_override(spec.engine)).await {
        log::error!("démarrage moteur : {e}");
    }
    notify_changed(&app);
}

// ---------------------------------------------------------------------------
// Dictée
// ---------------------------------------------------------------------------

fn on_hotkey(app: &AppHandle, ev: ShortcutState) {
    let state = app.state::<AppState>();
    let mode = state.settings().mode;
    let recording = state.is_recording();

    if ev == ShortcutState::Pressed {
        // Ignore la répétition clavier quand la touche reste enfoncée.
        let mut last = state.last_press.lock().unwrap();
        if let Some(t) = *last {
            if t.elapsed() < Duration::from_millis(350) {
                return;
            }
        }
        *last = Some(Instant::now());
    }

    match (mode, ev, recording) {
        (ShortcutMode::Toggle, ShortcutState::Pressed, false) => start_recording(app.clone()),
        (ShortcutMode::Toggle, ShortcutState::Pressed, true) => stop_and_transcribe(app.clone()),
        (ShortcutMode::Hold, ShortcutState::Pressed, false) => start_recording(app.clone()),
        (ShortcutMode::Hold, ShortcutState::Released, true) => stop_and_transcribe(app.clone()),
        _ => {}
    }
}

/// Mode direct : remplace le texte provisoire tapé dans le champ par `new_partial`
/// (efface l'ancien, tape le nouveau), en ne retapant que la partie qui change.
async fn retype_partial(app: &AppHandle, session: &Session, new_partial: &str) {
    let old = session.live.lock().unwrap().typed_partial.clone();
    if old == new_partial {
        return;
    }
    // Préfixe commun (en graphèmes) pour limiter les frappes.
    let common: String = {
        use unicode_segmentation::UnicodeSegmentation;
        let a: Vec<&str> = old.graphemes(true).collect();
        let b: Vec<&str> = new_partial.graphemes(true).collect();
        let n = a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count();
        b[..n].concat()
    };
    let to_delete = paste::grapheme_len(&old) - paste::grapheme_len(&common);
    let to_type = new_partial[common.len()..].to_string();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = app.run_on_main_thread(move || {
        let r = paste::backspace(to_delete).and_then(|_| paste::type_text(&to_type));
        let _ = tx.send(r);
    });
    match rx.await {
        Ok(Ok(())) => session.live.lock().unwrap().typed_partial = new_partial.to_string(),
        Ok(Err(e)) => log::warn!("frappe directe : {e}"),
        Err(_) => {}
    }
}

fn join_text(a: &str, b: &str) -> String {
    let (a, b) = (a.trim(), b.trim());
    if a.is_empty() {
        b.to_string()
    } else if b.is_empty() {
        a.to_string()
    } else {
        format!("{a} {b}")
    }
}

fn start_recording(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let eng = state.engine.lock().await.clone();
        let ready = match &eng {
            Some(e) => e.status().await.state == EngineState::Ready,
            None => false,
        };
        let Some(eng) = eng.filter(|_| ready) else {
            let starting = match state.engine.lock().await.as_ref() {
                Some(e) => e.status().await.state == EngineState::Starting,
                None => false,
            };
            overlay_error(&app, if starting { "Le modèle se charge encore…" } else { "Modèle non prêt — ouvrez Murmure" });
            return;
        };
        if state.is_recording() {
            return;
        }
        let want_direct = state.settings().auto_paste;
        let trusted = paste::accessibility_trusted();
        let direct = if want_direct && trusted {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let _ = app.run_on_main_thread(move || {
                let _ = tx.send(paste::focused_text_field());
            });
            rx.await.unwrap_or(false)
        } else {
            false
        };
        let hint = if direct {
            "direct"
        } else if want_direct && !trusted {
            "noperm"
        } else {
            ""
        };
        let app2 = app.clone();
        let started = tauri::async_runtime::spawn_blocking(move || {
            audio::Recorder::start(move |lvl| {
                if let Some(w) = app2.get_webview_window("overlay") {
                    let _ = w.emit("level", lvl);
                }
            })
        })
        .await;
        match started {
            Ok(Ok(recorder)) => {
                let id = state.session_seq.fetch_add(1, Ordering::SeqCst) + 1;
                let session = Arc::new(Session {
                    id,
                    recorder,
                    live: Mutex::new(LiveState::default()),
                    direct,
                });
                *state.session.lock().unwrap() = Some(session.clone());
                overlay_show(&app, "recording", "", hint);
                notify_changed(&app);
                let handle = tauri::async_runtime::spawn(live_loop(app.clone(), session, eng));
                *state.live_task.lock().await = Some(handle);
            }
            Ok(Err(e)) => overlay_error(&app, &format!("Micro : {e}")),
            Err(e) => overlay_error(&app, &format!("Micro : {e}")),
        }
    });
}

/// Transcription incrémentale pendant l'enregistrement : le segment en cours
/// est retranscrit régulièrement (texte provisoire) et validé dès qu'une pause
/// est détectée, pour que le texte apparaisse au fil de la parole.
async fn live_loop(app: AppHandle, session: Arc<Session>, eng: Arc<dyn Engine>) {
    const TICK_MS: u64 = 450;
    const MIN_PARTIAL_S: f32 = 1.0;
    const MIN_COMMIT_S: f32 = 3.5;
    const FORCE_COMMIT_S: f32 = 18.0;
    let rate = session.recorder.sample_rate();
    let secs = |n: usize| n as f32 / rate as f32;
    let state = app.state::<AppState>();
    let mut last_partial_len = 0usize;

    loop {
        tokio::time::sleep(Duration::from_millis(TICK_MS)).await;
        if !state.session_active(session.id) {
            break;
        }
        let (boundary, committed) = {
            let l = session.live.lock().unwrap();
            (l.boundary, l.committed.clone())
        };
        let tail = session.recorder.snapshot(boundary);
        let tail_s = secs(tail.len());
        if tail_s < MIN_PARTIAL_S {
            continue;
        }

        // 1. Valider un segment si une pause nette le permet.
        let mut cut: Option<usize> = None;
        if tail_s >= MIN_COMMIT_S {
            cut = audio::find_pause(&tail, rate, 550, 1.5);
            if cut.is_none() && tail_s >= FORCE_COMMIT_S {
                cut = Some(audio::quietest_point(&tail, rate, 4.0));
            }
        }
        if let Some(c) = cut.filter(|c| secs(*c) >= 1.0) {
            let chunk = &tail[..c];
            if !audio::has_speech(chunk, rate) {
                // Silence : on avance sans rien demander au modèle.
                let mut l = session.live.lock().unwrap();
                l.boundary = boundary + c;
                l.partial.clear();
                last_partial_len = 0;
                continue;
            }
            if let Ok(wav) = audio::to_wav_16k(chunk, rate) {
                match eng.transcribe(&wav).await {
                    Ok(t) => {
                        let seg = t.text.trim().to_string();
                        let committed_now = {
                            let mut l = session.live.lock().unwrap();
                            l.committed = join_text(&l.committed, &seg);
                            l.boundary = boundary + c;
                            l.partial.clear();
                            l.committed.clone()
                        };
                        last_partial_len = 0;
                        if session.direct {
                            if !seg.is_empty() {
                                retype_partial(&app, &session, &format!("{seg} ")).await;
                            } else {
                                retype_partial(&app, &session, "").await;
                            }
                            session.live.lock().unwrap().typed_partial.clear();
                            overlay_live(&app, "", "");
                        } else {
                            overlay_live(&app, &committed_now, "");
                        }
                    }
                    Err(e) => log::warn!("segment : {e}"),
                }
                if !state.session_active(session.id) {
                    break;
                }
                continue;
            }
        }

        // 2. Texte provisoire du segment en cours (si de l'audio nouveau existe).
        if tail.len() < last_partial_len + rate as usize / 2 {
            continue;
        }
        if !audio::has_speech(&tail, rate) {
            let had = {
                let mut l = session.live.lock().unwrap();
                let had = !l.partial.is_empty();
                l.partial.clear();
                had
            };
            if had {
                if session.direct {
                    retype_partial(&app, &session, "").await;
                }
                overlay_live(&app, if session.direct { "" } else { &committed }, "");
            }
            last_partial_len = tail.len();
            continue;
        }
        let Ok(wav) = audio::to_wav_16k(&tail, rate) else { continue };
        match eng.transcribe(&wav).await {
            Ok(t) => {
                if !state.session_active(session.id) {
                    break;
                }
                let partial = {
                    let mut l = session.live.lock().unwrap();
                    if l.boundary != boundary {
                        None
                    } else {
                        l.partial = t.text;
                        last_partial_len = tail.len();
                        Some(l.partial.clone())
                    }
                };
                if let Some(p) = partial {
                    if session.direct {
                        retype_partial(&app, &session, &p).await;
                    }
                    overlay_live(&app, if session.direct { "" } else { &committed }, &p);
                }
            }
            Err(e) => log::warn!("provisoire : {e}"),
        }
    }
}

fn stop_and_transcribe(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let Some(session) = state.session.lock().unwrap().take() else {
            return;
        };
        notify_changed(&app);
        let live = session.live.lock().unwrap().clone();
        overlay_show(&app, "transcribing", "", if session.direct { "direct" } else { "" });
        overlay_live(&app, if session.direct { "" } else { &live.committed }, &live.partial);

        let s2 = session.clone();
        let stopped = tauri::async_runtime::spawn_blocking(move || s2.recorder.stop()).await;
        if let Ok(Err(e)) = stopped {
            return overlay_error(&app, &format!("Capture : {e}"));
        }
        // Laisser la boucle incrémentale terminer sa requête en cours.
        if let Some(h) = state.live_task.lock().await.take() {
            let _ = h.await;
        }

        let rate = session.recorder.sample_rate();
        let total_s = session.recorder.len() as f32 / rate as f32;
        if total_s < 0.4 {
            return overlay_error(&app, "Enregistrement trop court");
        }
        let live = session.live.lock().unwrap().clone();
        let tail = session.recorder.snapshot(live.boundary);
        let tail_s = tail.len() as f32 / rate as f32;

        let eng = state.engine.lock().await.clone();
        let Some(eng) = eng else {
            return overlay_error(&app, "Moteur arrêté");
        };

        let mut tail_text = String::new();
        if tail_s >= 0.35 && audio::has_speech(&tail, rate) {
            let wav = match audio::to_wav_16k(&tail, rate) {
                Ok(w) => w,
                Err(e) => return overlay_error(&app, &format!("Audio : {e}")),
            };
            let committed = if session.direct { String::new() } else { live.committed.clone() };
            let app3 = app.clone();
            let on_partial = move |p: String| overlay_live(&app3, &committed, &p);
            match eng.transcribe_stream(&wav, &on_partial).await {
                Ok(t) => {
                    log::info!("fin de segment ({} ms, {:?})", t.duration_ms, t.language);
                    tail_text = t.text;
                }
                Err(e) => return overlay_error(&app, &format!("Transcription : {e}")),
            }
        }
        let full = join_text(&live.committed, &tail_text);
        if full.trim().is_empty() {
            return overlay_error(&app, "Rien n'a été reconnu");
        }

        if session.direct {
            // Les segments validés sont déjà dans le champ ; le provisoire tapé
            // est remplacé par la fin définitive.
            retype_partial(&app, &session, tail_text.trim()).await;
            overlay_show(&app, "done", "", "Dicté");
            overlay_hide_after(&app, 1200);
        } else {
            if let Err(e) = paste::copy(&full) {
                log::warn!("presse-papiers : {e}");
            }
            overlay_show(&app, "done", &full, "Copié dans le presse-papiers");
            overlay_hide_after(&app, 3000);
        }
    });
}

// ---------------------------------------------------------------------------
// Raccourci
// ---------------------------------------------------------------------------

fn apply_shortcut(app: &AppHandle, s: &str) -> Result<(), String> {
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    let sc: Shortcut = s
        .parse()
        .map_err(|e| format!("Raccourci invalide « {s} » : {e}"))?;
    gs.register(sc).map_err(|e| format!("Impossible d'enregistrer « {s} » : {e}"))
}

// ---------------------------------------------------------------------------
// Commandes exposées à l'interface
// ---------------------------------------------------------------------------

#[tauri::command]
async fn get_snapshot(state: tauri::State<'_, AppState>) -> Result<Snapshot, String> {
    Ok(snapshot(&state).await)
}

#[tauri::command]
async fn save_settings(app: AppHandle, state: tauri::State<'_, AppState>, settings: Settings) -> Result<Snapshot, String> {
    let old = state.settings();
    if old.shortcut != settings.shortcut {
        match apply_shortcut(&app, &settings.shortcut) {
            Ok(()) => *state.shortcut_error.lock().unwrap() = None,
            Err(e) => {
                // Revenir à l'ancien raccourci et signaler l'erreur.
                let _ = apply_shortcut(&app, &old.shortcut);
                return Err(e);
            }
        }
    }
    settings.save(&state.settings_path()).map_err(|e| e.to_string())?;
    *state.settings.lock().unwrap() = settings.clone();
    if old.model_id != settings.model_id || old.runtime_paths != settings.runtime_paths {
        tauri::async_runtime::spawn(start_engine(app.clone()));
    }
    Ok(snapshot(&state).await)
}

#[tauri::command]
async fn download_model(app: AppHandle, state: tauri::State<'_, AppState>, id: String) -> Result<(), String> {
    let spec = models::find(&id).ok_or("modèle inconnu")?;
    {
        let mut d = state.downloading.lock().unwrap();
        if !d.insert(id.clone()) {
            return Ok(());
        }
    }
    notify_changed(&app);
    let dir = models::model_dir(&state.models_root(), &id);
    tauri::async_runtime::spawn(async move {
        let res = models::download(&app, &spec, &dir).await;
        let st = app.state::<AppState>();
        st.downloading.lock().unwrap().remove(&id);
        match res {
            Ok(()) => {
                if st.settings().model_id == id {
                    start_engine(app.clone()).await;
                }
            }
            Err(e) => {
                log::error!("téléchargement {id} : {e}");
                models::emit(&app, &spec, 0, 0, false, Some(e.to_string()));
            }
        }
        notify_changed(&app);
    });
    Ok(())
}

#[tauri::command]
async fn delete_model(app: AppHandle, state: tauri::State<'_, AppState>, id: String) -> Result<Snapshot, String> {
    let spec = models::find(&id).ok_or("modèle inconnu")?;
    if state.settings().model_id == id {
        if let Some(e) = state.engine.lock().await.take() {
            e.stop().await;
        }
    }
    models::delete(&spec, &models::model_dir(&state.models_root(), &id)).map_err(|e| e.to_string())?;
    notify_changed(&app);
    Ok(snapshot(&state).await)
}

#[tauri::command]
async fn restart_engine(app: AppHandle) {
    tauri::async_runtime::spawn(start_engine(app));
}

#[tauri::command]
fn toggle_recording(app: AppHandle, state: tauri::State<'_, AppState>) {
    if state.is_recording() {
        stop_and_transcribe(app);
    } else {
        start_recording(app);
    }
}

#[tauri::command]
fn request_accessibility(app: AppHandle) {
    let _ = app.run_on_main_thread(|| paste::request_accessibility());
    #[cfg(target_os = "macos")]
    {
        use tauri_plugin_opener::OpenerExt;
        let _ = app
            .opener()
            .open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility", None::<&str>);
    }
}

#[tauri::command]
fn open_models_dir(app: AppHandle, state: tauri::State<'_, AppState>) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let dir = state.models_root();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    app.opener()
        .open_path(dir.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn open_engine_log(app: AppHandle, state: tauri::State<'_, AppState>) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let kind = models::find(&state.settings().model_id).map(|m| m.engine).unwrap_or(engine::EngineKind::LlamaCpp);
    let p = engine::sidecar::log_path(kind);
    app.opener()
        .open_path(p.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Application
// ---------------------------------------------------------------------------

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| on_hotkey(app, event.state()))
                .build(),
        )
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let settings = Settings::load(&data_dir.join("settings.json"));
            for k in engine::EngineKind::all() {
                engine::sidecar::kill_stale(k, k.binary());
            }
            app.manage(AppState {
                data_dir,
                settings: Mutex::new(settings.clone()),
                engine: tokio::sync::Mutex::new(None),
                session: Mutex::new(None),
                live_task: tokio::sync::Mutex::new(None),
                downloading: Mutex::new(HashSet::new()),
                last_press: Mutex::new(None),
                shortcut_error: Mutex::new(None),
                overlay_gen: AtomicU64::new(0),
                session_seq: AtomicU64::new(0),
            });

            if let Err(e) = apply_shortcut(app.handle(), &settings.shortcut) {
                log::error!("{e}");
                *app.state::<AppState>().shortcut_error.lock().unwrap() = Some(e);
            }

            if let Some(overlay) = app.get_webview_window("overlay") {
                let _ = overlay.set_ignore_cursor_events(true);
            }

            // Barre des menus / zone de notification.
            let open = MenuItem::with_id(app, "open", "Ouvrir Murmure", true, None::<&str>)?;
            let dictate = MenuItem::with_id(app, "dictate", "Démarrer / arrêter la dictée", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quitter", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &dictate, &PredefinedMenuItem::separator(app)?, &quit])?;
            let tray_icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
            TrayIconBuilder::with_id("main")
                .icon(tray_icon)
                .icon_as_template(true)
                .tooltip("Murmure")
                .menu(&menu)
                .show_menu_on_left_click(true)
                .on_menu_event(|app, ev| match ev.id().as_ref() {
                    "open" => show_main(app),
                    "dictate" => {
                        if app.state::<AppState>().is_recording() {
                            stop_and_transcribe(app.clone());
                        } else {
                            start_recording(app.clone());
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, ev| {
                    if let TrayIconEvent::DoubleClick { .. } = ev {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;

            tauri::async_runtime::spawn(start_engine(app.handle().clone()));
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            save_settings,
            download_model,
            delete_model,
            restart_engine,
            toggle_recording,
            open_models_dir,
            open_engine_log,
            request_accessibility
        ])
        .build(tauri::generate_context!())
        .expect("erreur au démarrage de Murmure");

    app.run(|app, event| match event {
        RunEvent::Exit => {
            let state = app.state::<AppState>();
            tauri::async_runtime::block_on(async {
                if let Some(e) = state.engine.lock().await.take() {
                    e.stop().await;
                }
            });
        }
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => show_main(app),
        _ => {}
    });
}
