//! Catalogue des modèles et téléchargement des poids.
//!
//! Pour ajouter un modèle : une entrée dans `catalog()`. Chaque modèle déclare
//! le moteur qui sait l'exécuter (`EngineKind`) et la liste des fichiers à
//! récupérer. Sélection basée sur l'Open ASR Leaderboard (modèles open-weight
//! exécutables en local sur Mac et Windows).

use crate::engine::EngineKind;
use futures_util::StreamExt;
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter};

#[derive(Debug, Clone, Serialize)]
pub struct RemoteFile {
    pub url: String,
    pub name: String,
}

/// Forme de la sortie brute du modèle.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum OutputFormat {
    /// `language French<asr_text>Bonjour…` (Qwen3-ASR).
    Qwen3Asr,
    /// Texte brut.
    Plain,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelSpec {
    pub id: String,
    pub name: String,
    pub description: String,
    pub size_label: String,
    pub params: String,
    pub languages: String,
    pub engine: EngineKind,
    pub files: Vec<RemoteFile>,
    /// Fichier principal (poids du décodeur).
    pub main_file: String,
    /// Projecteur multimodal (encodeur audio) pour llama.cpp.
    pub mmproj_file: Option<String>,
    /// Consigne texte envoyée avec l'audio (modèles conversationnels type Voxtral).
    pub prompt: Option<String>,
    pub output: OutputFormat,
}

fn hf(repo: &str, file: &str) -> RemoteFile {
    RemoteFile {
        url: format!("https://huggingface.co/{repo}/resolve/main/{file}"),
        name: file.to_string(),
    }
}

fn gh(url: &str) -> RemoteFile {
    RemoteFile {
        url: url.to_string(),
        name: url.rsplit('/').next().unwrap_or("file").to_string(),
    }
}

pub fn catalog() -> Vec<ModelSpec> {
    let mut v = vec![
        ModelSpec {
            id: "qwen3-asr-1.7b".into(),
            name: "Qwen3-ASR 1.7B".into(),
            description: "Meilleur compromis précision / vitesse. Détection automatique de la langue, robuste au bruit et au chant.".into(),
            size_label: "2,5 Go".into(),
            params: "1,7 Md".into(),
            languages: "30 langues + 22 dialectes chinois".into(),
            engine: EngineKind::LlamaCpp,
            files: vec![
                hf("ggml-org/Qwen3-ASR-1.7B-GGUF", "Qwen3-ASR-1.7B-Q8_0.gguf"),
                hf("ggml-org/Qwen3-ASR-1.7B-GGUF", "mmproj-Qwen3-ASR-1.7B-Q8_0.gguf"),
            ],
            main_file: "Qwen3-ASR-1.7B-Q8_0.gguf".into(),
            mmproj_file: Some("mmproj-Qwen3-ASR-1.7B-Q8_0.gguf".into()),
            prompt: None,
            output: OutputFormat::Qwen3Asr,
        },
        ModelSpec {
            id: "qwen3-asr-0.6b".into(),
            name: "Qwen3-ASR 0.6B".into(),
            description: "Version légère de Qwen3-ASR : même couverture linguistique, plus rapide, un peu moins précise.".into(),
            size_label: "1,1 Go".into(),
            params: "0,6 Md".into(),
            languages: "30 langues + 22 dialectes chinois".into(),
            engine: EngineKind::LlamaCpp,
            files: vec![
                hf("ggml-org/Qwen3-ASR-0.6B-GGUF", "Qwen3-ASR-0.6B-Q8_0.gguf"),
                hf("ggml-org/Qwen3-ASR-0.6B-GGUF", "mmproj-Qwen3-ASR-0.6B-Q8_0.gguf"),
            ],
            main_file: "Qwen3-ASR-0.6B-Q8_0.gguf".into(),
            mmproj_file: Some("mmproj-Qwen3-ASR-0.6B-Q8_0.gguf".into()),
            prompt: None,
            output: OutputFormat::Qwen3Asr,
        },
        ModelSpec {
            id: "voxtral-mini-3b".into(),
            name: "Voxtral Mini 3B".into(),
            description: "Modèle Mistral, le plus lourd du catalogue. Très bon sur les langues européennes, plus lent.".into(),
            size_label: "2,9 Go".into(),
            params: "3 Md".into(),
            languages: "EN, FR, ES, DE, IT, PT, NL, HI + autres".into(),
            engine: EngineKind::LlamaCpp,
            files: vec![
                hf("ggml-org/Voxtral-Mini-3B-2507-GGUF", "Voxtral-Mini-3B-2507-Q4_K_M.gguf"),
                hf("ggml-org/Voxtral-Mini-3B-2507-GGUF", "mmproj-Voxtral-Mini-3B-2507-Q8_0.gguf"),
            ],
            main_file: "Voxtral-Mini-3B-2507-Q4_K_M.gguf".into(),
            mmproj_file: Some("mmproj-Voxtral-Mini-3B-2507-Q8_0.gguf".into()),
            prompt: Some("Transcribe this audio verbatim in its original language. Output only the transcription, nothing else.".into()),
            output: OutputFormat::Plain,
        },
        ModelSpec {
            id: "whisper-large-v3-turbo".into(),
            name: "Whisper large-v3 turbo".into(),
            description: "OpenAI Whisper accéléré (décodeur réduit). Très large couverture linguistique, rapide.".into(),
            size_label: "874 Mo".into(),
            params: "0,8 Md".into(),
            languages: "99 langues".into(),
            engine: EngineKind::WhisperCpp,
            files: vec![hf("ggerganov/whisper.cpp", "ggml-large-v3-turbo-q8_0.bin")],
            main_file: "ggml-large-v3-turbo-q8_0.bin".into(),
            mmproj_file: None,
            prompt: None,
            output: OutputFormat::Plain,
        },
        ModelSpec {
            id: "whisper-large-v3".into(),
            name: "Whisper large-v3".into(),
            description: "Référence multilingue d'OpenAI, plus précise que turbo mais nettement plus lente.".into(),
            size_label: "1,1 Go".into(),
            params: "1,55 Md".into(),
            languages: "99 langues".into(),
            engine: EngineKind::WhisperCpp,
            files: vec![hf("ggerganov/whisper.cpp", "ggml-large-v3-q5_0.bin")],
            main_file: "ggml-large-v3-q5_0.bin".into(),
            mmproj_file: None,
            prompt: None,
            output: OutputFormat::Plain,
        },
    ];
    v.extend(sherpa_models());
    v
}

/// Modèles NVIDIA NeMo exécutés par sherpa-onnx (archives pré-exportées).
fn sherpa_models() -> Vec<ModelSpec> {
    const REL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models";
    vec![
        ModelSpec {
            id: "parakeet-tdt-0.6b-v3".into(),
            name: "Parakeet TDT 0.6B v3".into(),
            description: "NVIDIA. Le plus rapide du catalogue, en tête du leaderboard en débit. Fonctionne sur CPU.".into(),
            size_label: "487 Mo".into(),
            params: "0,6 Md".into(),
            languages: "25 langues européennes".into(),
            engine: EngineKind::SherpaOnnx,
            files: vec![gh(&format!("{REL}/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2"))],
            main_file: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2".into(),
            mmproj_file: None,
            prompt: None,
            output: OutputFormat::Plain,
        },
        ModelSpec {
            id: "canary-180m-flash".into(),
            name: "Canary 180M flash".into(),
            description: "NVIDIA. Ultra-léger, excellent en anglais, espagnol, allemand et français.".into(),
            size_label: "154 Mo".into(),
            params: "0,18 Md".into(),
            languages: "EN, ES, DE, FR".into(),
            engine: EngineKind::SherpaOnnx,
            files: vec![gh(&format!("{REL}/sherpa-onnx-nemo-canary-180m-flash-en-es-de-fr-int8.tar.bz2"))],
            main_file: "sherpa-onnx-nemo-canary-180m-flash-en-es-de-fr-int8.tar.bz2".into(),
            mmproj_file: None,
            prompt: None,
            output: OutputFormat::Plain,
        },
    ]
}

pub fn find(id: &str) -> Option<ModelSpec> {
    catalog().into_iter().find(|m| m.id == id)
}

pub fn model_dir(models_root: &Path, id: &str) -> PathBuf {
    models_root.join(id)
}

pub fn is_downloaded(spec: &ModelSpec, dir: &Path) -> bool {
    spec.files.iter().all(|f| {
        dir.join(&f.name)
            .metadata()
            .map(|m| m.len() > 1024)
            .unwrap_or(false)
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct DownloadProgress {
    pub id: String,
    pub downloaded: u64,
    pub total: u64,
    pub done: bool,
    pub error: Option<String>,
}

/// Télécharge tous les fichiers d'un modèle, avec reprise (Range) et
/// événements `model-progress` vers l'interface.
pub async fn download(app: &AppHandle, spec: &ModelSpec, dir: &Path) -> anyhow::Result<()> {
    tokio::fs::create_dir_all(dir).await?;
    let client = reqwest::Client::builder()
        .user_agent("murmure/0.1")
        .build()?;

    let mut totals = Vec::new();
    for f in &spec.files {
        let head = client.head(&f.url).send().await?;
        totals.push(head.content_length().unwrap_or(0));
    }
    let total: u64 = totals.iter().sum();
    let mut downloaded: u64 = 0;

    for (f, expected) in spec.files.iter().zip(totals) {
        let dest = dir.join(&f.name);
        if dest.metadata().map(|m| m.len() == expected && expected > 0).unwrap_or(false) {
            downloaded += expected;
            emit(app, spec, downloaded, total, false, None);
            continue;
        }
        let part = dir.join(format!("{}.part", f.name));
        let mut have = part.metadata().map(|m| m.len()).unwrap_or(0);
        if have > expected {
            tokio::fs::remove_file(&part).await.ok();
            have = 0;
        }

        let mut req = client.get(&f.url);
        if have > 0 {
            req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
        }
        let resp = req.send().await?.error_for_status()?;
        let resumed = resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;
        if !resumed {
            have = 0;
        }

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(resumed)
            .truncate(!resumed)
            .open(&part)
            .await?;

        downloaded += have;
        let mut stream = resp.bytes_stream();
        let mut last_emit = std::time::Instant::now();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await?;
            downloaded += chunk.len() as u64;
            if last_emit.elapsed().as_millis() > 150 {
                emit(app, spec, downloaded, total, false, None);
                last_emit = std::time::Instant::now();
            }
        }
        tokio::io::AsyncWriteExt::flush(&mut file).await?;
        drop(file);
        tokio::fs::rename(&part, &dest).await?;
    }
    emit(app, spec, total, total, true, None);
    Ok(())
}

pub fn emit(app: &AppHandle, spec: &ModelSpec, downloaded: u64, total: u64, done: bool, error: Option<String>) {
    let _ = app.emit(
        "model-progress",
        DownloadProgress {
            id: spec.id.clone(),
            downloaded,
            total,
            done,
            error,
        },
    );
}

pub fn delete(_spec: &ModelSpec, dir: &Path) -> anyhow::Result<()> {
    if dir.is_dir() {
        std::fs::remove_dir_all(dir)?;
    }
    Ok(())
}
