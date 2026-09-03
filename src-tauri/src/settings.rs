use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ShortcutMode {
    /// Appuyer une fois pour démarrer, une seconde fois pour arrêter.
    Toggle,
    /// Maintenir le raccourci pendant la dictée.
    Hold,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub model_id: String,
    pub shortcut: String,
    pub mode: ShortcutMode,
    /// Dicter directement dans le champ texte actif (sinon : presse-papiers + overlay).
    pub auto_paste: bool,
    /// Chemins explicites vers les binaires des moteurs, par identifiant de moteur
    /// (`llama-cpp`, `whisper-cpp`, …). Sinon détection automatique.
    pub runtime_paths: BTreeMap<String, String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            model_id: "qwen3-asr-1.7b".into(),
            shortcut: "Ctrl+Alt+Space".into(),
            mode: ShortcutMode::Toggle,
            auto_paste: true,
            runtime_paths: BTreeMap::new(),
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    pub fn runtime_override(&self, kind: crate::engine::EngineKind) -> Option<&str> {
        self.runtime_paths.get(kind.id()).map(|s| s.as_str()).filter(|s| !s.trim().is_empty())
    }
}
