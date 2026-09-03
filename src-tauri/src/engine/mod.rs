//! Abstraction moteur d'inférence. Un moteur sait démarrer un modèle du
//! catalogue et transcrire un WAV. Pour brancher un autre runtime, implémenter
//! `Engine`, ajouter une variante à `EngineKind` et la brancher dans `for_kind`.

pub mod llama;
pub mod sherpa;
pub mod whisper;

use crate::models::ModelSpec;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum EngineKind {
    LlamaCpp,
    WhisperCpp,
    SherpaOnnx,
}

impl EngineKind {
    pub fn all() -> [EngineKind; 3] {
        [EngineKind::LlamaCpp, EngineKind::WhisperCpp, EngineKind::SherpaOnnx]
    }
    pub fn id(&self) -> &'static str {
        match self {
            EngineKind::LlamaCpp => "llama-cpp",
            EngineKind::WhisperCpp => "whisper-cpp",
            EngineKind::SherpaOnnx => "sherpa-onnx",
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            EngineKind::LlamaCpp => "llama.cpp",
            EngineKind::WhisperCpp => "whisper.cpp",
            EngineKind::SherpaOnnx => "sherpa-onnx",
        }
    }
    /// Nom du binaire serveur attendu.
    pub fn binary(&self) -> &'static str {
        match self {
            EngineKind::LlamaCpp => "llama-server",
            EngineKind::WhisperCpp => "whisper-server",
            EngineKind::SherpaOnnx => "sherpa-onnx-offline-websocket-server",
        }
    }
    pub fn env_var(&self) -> &'static str {
        match self {
            EngineKind::LlamaCpp => "MURMURE_LLAMA_SERVER",
            EngineKind::WhisperCpp => "MURMURE_WHISPER_SERVER",
            EngineKind::SherpaOnnx => "MURMURE_SHERPA_ONNX",
        }
    }
    /// Comment installer le runtime si absent.
    pub fn install_hint(&self) -> &'static str {
        match self {
            EngineKind::LlamaCpp => if cfg!(target_os = "macos") { "brew install llama.cpp" } else { "Téléchargez llama.cpp (release GitHub) et indiquez le chemin de llama-server.exe" },
            EngineKind::WhisperCpp => if cfg!(target_os = "macos") { "brew install whisper-cpp" } else { "Téléchargez whisper.cpp (release GitHub) et indiquez le chemin de whisper-server.exe" },
            EngineKind::SherpaOnnx => "Téléchargé automatiquement au premier démarrage",
        }
    }
}

/// Cherche un binaire de runtime : chemin forcé, variable d'environnement,
/// dossier de runtimes de l'app, à côté de l'exécutable, PATH, Homebrew.
pub fn find_binary(kind: EngineKind, override_path: Option<&str>, data_dir: Option<&Path>) -> Option<PathBuf> {
    let bin = if cfg!(windows) { format!("{}.exe", kind.binary()) } else { kind.binary().to_string() };
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(p) = override_path.filter(|p| !p.trim().is_empty()) {
        candidates.push(PathBuf::from(p));
    }
    if let Ok(p) = std::env::var(kind.env_var()) {
        candidates.push(PathBuf::from(p));
    }
    if let Some(d) = data_dir {
        candidates.push(d.join("runtimes").join(kind.id()).join("bin").join(&bin));
        candidates.push(d.join("runtimes").join(kind.id()).join(&bin));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("runtimes").join(kind.id()).join(&bin));
            candidates.push(dir.join("../Resources/runtimes").join(kind.id()).join(&bin));
            candidates.push(dir.join(&bin));
            candidates.push(dir.join("bin").join(&bin));
        }
    }
    if let Ok(path) = std::env::var("PATH") {
        for p in std::env::split_paths(&path) {
            candidates.push(p.join(&bin));
        }
    }
    candidates.push(PathBuf::from("/opt/homebrew/bin").join(&bin));
    candidates.push(PathBuf::from("/usr/local/bin").join(&bin));
    candidates.into_iter().find(|p| p.is_file())
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum EngineState {
    Stopped,
    Starting,
    Ready,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct EngineStatus {
    pub state: EngineState,
    pub message: String,
    pub model_id: Option<String>,
    pub port: Option<u16>,
    pub runtime_path: Option<String>,
}

impl EngineStatus {
    pub fn stopped() -> Self {
        Self {
            state: EngineState::Stopped,
            message: "Moteur arrêté".into(),
            model_id: None,
            port: None,
            runtime_path: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Transcript {
    pub text: String,
    pub language: Option<String>,
    pub duration_ms: u128,
}

#[async_trait::async_trait]
pub trait Engine: Send + Sync {
    fn kind(&self) -> EngineKind;
    /// Démarre le moteur pour `spec` dont les fichiers sont dans `dir`.
    async fn start(&self, spec: &ModelSpec, dir: &Path, runtime_override: Option<&str>) -> anyhow::Result<()>;
    async fn stop(&self);
    async fn status(&self) -> EngineStatus;
    async fn transcribe(&self, wav: &[u8]) -> anyhow::Result<Transcript>;
    /// Transcription avec texte partiel au fil des tokens (`on_partial` reçoit
    /// le texte cumulé, déjà nettoyé de l'en-tête de langue).
    async fn transcribe_stream(&self, wav: &[u8], on_partial: &(dyn Fn(String) + Send + Sync)) -> anyhow::Result<Transcript>;
}

pub fn for_kind(kind: EngineKind, data_dir: &Path) -> Arc<dyn Engine> {
    match kind {
        EngineKind::LlamaCpp => Arc::new(llama::LlamaEngine::new(data_dir.to_path_buf())),
        EngineKind::WhisperCpp => Arc::new(whisper::WhisperEngine::new(data_dir.to_path_buf())),
        EngineKind::SherpaOnnx => Arc::new(sherpa::SherpaEngine::new(data_dir.to_path_buf())),
    }
}

/// Utilitaires partagés par les moteurs à sous-processus.
pub mod sidecar {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    pub fn free_port() -> anyhow::Result<u16> {
        let l = std::net::TcpListener::bind("127.0.0.1:0")?;
        Ok(l.local_addr()?.port())
    }

    pub fn log_path(kind: super::EngineKind) -> PathBuf {
        std::env::temp_dir().join(format!("murmure-{}.log", kind.id()))
    }

    pub fn pid_path(kind: super::EngineKind) -> PathBuf {
        std::env::temp_dir().join(format!("murmure-{}.pid", kind.id()))
    }

    pub fn log_tail(path: &Path, n: usize) -> String {
        std::fs::read_to_string(path)
            .map(|s| {
                let lines: Vec<&str> = s.lines().collect();
                lines[lines.len().saturating_sub(n)..].join("\n")
            })
            .unwrap_or_default()
    }

    /// Tue un serveur orphelin laissé par une précédente instance (crash).
    pub fn kill_stale(kind: super::EngineKind, marker: &str) {
        let pid_file = pid_path(kind);
        let Ok(s) = std::fs::read_to_string(&pid_file) else { return };
        let Ok(pid) = s.trim().parse::<u32>() else { return };
        let ours = if cfg!(windows) {
            Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase().contains(kind.binary()))
                .unwrap_or(false)
        } else {
            Command::new("ps")
                .args(["-p", &pid.to_string(), "-o", "command="])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).contains(marker))
                .unwrap_or(false)
        };
        if ours {
            log::warn!("{} orphelin (pid {pid}) : arrêt", kind.binary());
            if cfg!(windows) {
                let _ = Command::new("taskkill").args(["/PID", &pid.to_string(), "/F"]).output();
            } else {
                let _ = Command::new("kill").arg(pid.to_string()).output();
            }
        }
        let _ = std::fs::remove_file(pid_file);
    }

    #[cfg(windows)]
    pub fn hide_console(cmd: &mut Command) {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    pub fn hide_console(_cmd: &mut Command) {}
}
