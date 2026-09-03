//! Catalogue des modèles et téléchargement des poids.
//!
//! Pour ajouter un modèle : une entrée dans `catalog()`. Chaque modèle déclare
//! le moteur qui sait l'exécuter (`EngineKind`) et la liste des fichiers à
//! récupérer. Sélection basée sur l'Open ASR Leaderboard (modèles open-weight
//! exécutables en local sur Mac et Windows).

use crate::engine::EngineKind;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, RwLock};
use tauri::{AppHandle, Emitter};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteFile {
    /// URL http(s), ou chemin local absolu (copié dans le dossier du modèle).
    pub url: String,
    pub name: String,
}

/// Forme de la sortie brute du modèle.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum OutputFormat {
    /// `language French<asr_text>Bonjour…` (Qwen3-ASR).
    Qwen3Asr,
    /// Texte brut.
    Plain,
}

/// Scores publics (Open ASR Leaderboard, Hugging Face). WER en %, plus bas = mieux.
/// `rtfx` : vitesse de référence du leaderboard (×temps réel sur GPU A100), utile
/// pour comparer les modèles entre eux, pas pour prédire la vitesse locale.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Bench {
    pub wer_en: Option<f32>,
    pub wer_fr: Option<f32>,
    pub wer_fr_cv: Option<f32>,
    pub rtfx: Option<f32>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSpec {
    pub id: String,
    pub name: String,
    pub description: String,
    pub size_label: String,
    pub params: String,
    pub languages: String,
    pub license: String,
    pub engine: EngineKind,
    pub files: Vec<RemoteFile>,
    /// Fichier principal (poids du décodeur).
    pub main_file: String,
    /// Projecteur multimodal (encodeur audio) pour llama.cpp.
    pub mmproj_file: Option<String>,
    /// Consigne texte envoyée avec l'audio (modèles conversationnels type Voxtral).
    pub prompt: Option<String>,
    pub output: OutputFormat,
    #[serde(default)]
    pub bench: Option<Bench>,
    #[serde(default)]
    pub recommended: bool,
    #[serde(default)]
    pub custom: bool,
}

fn bench(wer_en: f32, wer_fr: Option<f32>, wer_fr_cv: Option<f32>, rtfx: f32, note: Option<&str>) -> Option<Bench> {
    Some(Bench {
        wer_en: Some(wer_en),
        wer_fr,
        wer_fr_cv,
        rtfx: Some(rtfx),
        note: note.map(|s| s.to_string()),
    })
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

static CUSTOM: LazyLock<RwLock<Vec<ModelSpec>>> = LazyLock::new(|| RwLock::new(Vec::new()));

fn custom_path(data_dir: &Path) -> PathBuf {
    data_dir.join("custom_models.json")
}

/// Charge les modèles personnalisés depuis le disque (au démarrage).
pub fn load_custom(data_dir: &Path) {
    let list: Vec<ModelSpec> = std::fs::read(custom_path(data_dir))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    *CUSTOM.write().unwrap() = list;
}

fn save_custom(data_dir: &Path) -> anyhow::Result<()> {
    let list = CUSTOM.read().unwrap().clone();
    std::fs::write(custom_path(data_dir), serde_json::to_vec_pretty(&list)?)?;
    Ok(())
}

/// Saisie d'un modèle personnalisé depuis l'interface.
#[derive(Debug, Clone, Deserialize)]
pub struct CustomModelInput {
    pub name: String,
    pub engine: EngineKind,
    /// URL http(s) ou chemins locaux ; le premier est le fichier principal.
    pub files: Vec<String>,
    pub prompt: Option<String>,
    pub output: OutputFormat,
    pub languages: Option<String>,
}

fn slug(s: &str) -> String {
    let mut out: String = s
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out.trim_matches('-').to_string()
}

fn file_name_of(url: &str) -> String {
    url.trim_end_matches('/')
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("fichier")
        .split('?')
        .next()
        .unwrap_or("fichier")
        .to_string()
}

pub fn add_custom(data_dir: &Path, input: CustomModelInput) -> anyhow::Result<ModelSpec> {
    let name = input.name.trim();
    if name.is_empty() {
        anyhow::bail!("Donnez un nom au modèle.");
    }
    let files: Vec<String> = input.files.iter().map(|f| f.trim().to_string()).filter(|f| !f.is_empty()).collect();
    if files.is_empty() {
        anyhow::bail!("Indiquez au moins le fichier principal (URL ou chemin).");
    }
    for f in &files {
        let ok = f.starts_with("http://") || f.starts_with("https://") || Path::new(f).is_absolute();
        if !ok {
            anyhow::bail!("« {f} » n'est ni une URL ni un chemin absolu.");
        }
    }
    let mut id = format!("custom-{}", slug(name));
    if find(&id).is_some() {
        id = format!("{id}-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() % 100000).unwrap_or(0));
    }
    let remote: Vec<RemoteFile> = files
        .iter()
        .map(|f| RemoteFile {
            url: f.clone(),
            name: file_name_of(f),
        })
        .collect();
    let main_file = remote[0].name.clone();
    let mmproj_file = if input.engine == EngineKind::LlamaCpp {
        remote.iter().skip(1).find(|f| f.name.to_lowercase().contains("mmproj")).or(remote.get(1)).map(|f| f.name.clone())
    } else {
        None
    };
    let spec = ModelSpec {
        id,
        name: name.to_string(),
        description: "Modèle ajouté manuellement. Pas de score de référence.".into(),
        size_label: "—".into(),
        params: "—".into(),
        languages: input.languages.filter(|l| !l.trim().is_empty()).unwrap_or_else(|| "—".into()),
        license: "—".into(),
        engine: input.engine,
        files: remote,
        main_file,
        mmproj_file,
        prompt: input.prompt.filter(|p| !p.trim().is_empty()),
        output: input.output,
        bench: None,
        recommended: false,
        custom: true,
    };
    CUSTOM.write().unwrap().push(spec.clone());
    save_custom(data_dir)?;
    Ok(spec)
}

pub fn remove_custom(data_dir: &Path, id: &str) -> anyhow::Result<()> {
    CUSTOM.write().unwrap().retain(|m| m.id != id);
    save_custom(data_dir)
}

pub fn catalog() -> Vec<ModelSpec> {
    let mut v = builtin();
    v.extend(CUSTOM.read().unwrap().iter().cloned());
    v
}

fn builtin() -> Vec<ModelSpec> {
    let mut v = vec![
        ModelSpec {
            id: "qwen3-asr-1.7b".into(),
            name: "Qwen3-ASR 1.7B".into(),
            description: "Le plus précis du catalogue en anglais comme en français (1er de l'Open ASR Leaderboard parmi les modèles ouverts exécutables ici). Détection automatique de la langue.".into(),
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
            license: "Apache 2.0".into(),
            bench: bench(4.31, Some(4.06), Some(7.84), 820.0, None),
            recommended: true,
            custom: false,
            output: OutputFormat::Qwen3Asr,
        },
        ModelSpec {
            id: "qwen3-asr-0.6b".into(),
            name: "Qwen3-ASR 0.6B".into(),
            description: "Version légère de Qwen3-ASR. Bon en anglais, nettement moins précis en français que le 1.7B.".into(),
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
            license: "Apache 2.0".into(),
            bench: bench(5.05, Some(7.06), Some(10.78), 744.0, None),
            recommended: false,
            custom: false,
            output: OutputFormat::Qwen3Asr,
        },
        ModelSpec {
            id: "voxtral-mini-3b".into(),
            name: "Voxtral Mini 3B".into(),
            description: "Modèle Mistral. Très bon en français, moyen en anglais, et le plus lent du catalogue.".into(),
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
            license: "Apache 2.0".into(),
            bench: bench(5.54, Some(4.13), Some(7.80), 181.0, Some("Poids quantifiés Q4_K_M : précision légèrement inférieure au score publié.")),
            recommended: false,
            custom: false,
            output: OutputFormat::Plain,
        },
        ModelSpec {
            id: "whisper-large-v3-turbo".into(),
            name: "Whisper large-v3 turbo".into(),
            description: "OpenAI Whisper accéléré. La plus large couverture linguistique, précision en retrait sur l'anglais.".into(),
            size_label: "874 Mo".into(),
            params: "0,8 Md".into(),
            languages: "99 langues".into(),
            engine: EngineKind::WhisperCpp,
            files: vec![hf("ggerganov/whisper.cpp", "ggml-large-v3-turbo-q8_0.bin")],
            main_file: "ggml-large-v3-turbo-q8_0.bin".into(),
            mmproj_file: None,
            prompt: None,
            license: "MIT".into(),
            bench: bench(6.36, Some(4.90), Some(11.06), 792.0, None),
            recommended: false,
            custom: false,
            output: OutputFormat::Plain,
        },
        ModelSpec {
            id: "whisper-large-v3".into(),
            name: "Whisper large-v3".into(),
            description: "Référence multilingue d'OpenAI. Plus précis que turbo, plus lent.".into(),
            size_label: "1,1 Go".into(),
            params: "1,55 Md".into(),
            languages: "99 langues".into(),
            engine: EngineKind::WhisperCpp,
            files: vec![hf("ggerganov/whisper.cpp", "ggml-large-v3-q5_0.bin")],
            main_file: "ggml-large-v3-q5_0.bin".into(),
            mmproj_file: None,
            prompt: None,
            license: "Apache 2.0".into(),
            bench: bench(5.78, Some(4.84), Some(9.97), 470.0, Some("Poids quantifiés Q5_0.")),
            recommended: false,
            custom: false,
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
            description: "NVIDIA. Le plus rapide du catalogue et très bon en français (meilleur score Common Voice). Tourne sur CPU.".into(),
            size_label: "487 Mo".into(),
            params: "0,6 Md".into(),
            languages: "25 langues européennes".into(),
            engine: EngineKind::SherpaOnnx,
            files: vec![gh(&format!("{REL}/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2"))],
            main_file: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2".into(),
            mmproj_file: None,
            prompt: None,
            license: "CC BY 4.0".into(),
            bench: bench(4.86, Some(4.68), Some(6.35), 6076.0, Some("Poids int8 : précision légèrement inférieure au score publié. Tourne sur CPU.")),
            recommended: false,
            custom: false,
            output: OutputFormat::Plain,
        },
        ModelSpec {
            id: "parakeet-tdt-0.6b-v2".into(),
            name: "Parakeet TDT 0.6B v2".into(),
            description: "NVIDIA. Version anglais seul, un peu plus précise que la v3 en anglais. Très rapide, sur CPU.".into(),
            size_label: "482 Mo".into(),
            params: "0,6 Md".into(),
            languages: "Anglais uniquement".into(),
            license: "CC BY 4.0".into(),
            engine: EngineKind::SherpaOnnx,
            files: vec![gh(&format!("{REL}/sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8.tar.bz2"))],
            main_file: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8.tar.bz2".into(),
            mmproj_file: None,
            prompt: None,
            bench: bench(4.70, None, None, 6025.0, Some("Poids int8.")),
            recommended: false,
            custom: false,
            output: OutputFormat::Plain,
        },
        ModelSpec {
            id: "canary-180m-flash".into(),
            name: "Canary 180M flash".into(),
            description: "NVIDIA. Ultra-léger et rapide. Bon en anglais ; langue à fixer à l'avance.".into(),
            size_label: "154 Mo".into(),
            params: "0,18 Md".into(),
            languages: "EN, ES, DE, FR".into(),
            engine: EngineKind::SherpaOnnx,
            files: vec![gh(&format!("{REL}/sherpa-onnx-nemo-canary-180m-flash-en-es-de-fr-int8.tar.bz2"))],
            main_file: "sherpa-onnx-nemo-canary-180m-flash-en-es-de-fr-int8.tar.bz2".into(),
            mmproj_file: None,
            prompt: None,
            license: "CC BY 4.0".into(),
            bench: bench(5.54, None, None, 2489.0, Some("Poids int8. Langue fixée (français par défaut), pas de détection automatique.")),
            recommended: false,
            custom: false,
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
        if is_local(&f.url) {
            totals.push(tokio::fs::metadata(&f.url).await.map(|m| m.len()).unwrap_or(0));
            continue;
        }
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
        if is_local(&f.url) {
            tokio::fs::copy(&f.url, &dest).await.map_err(|e| anyhow::anyhow!("copie de {} : {e}", f.url))?;
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

fn is_local(url: &str) -> bool {
    !(url.starts_with("http://") || url.starts_with("https://"))
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
