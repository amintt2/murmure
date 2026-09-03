//! Moteur sherpa-onnx : lance `sherpa-onnx-offline-websocket-server` en
//! sous-processus sur un port local et lui parle via le protocole websocket
//! « offline » de sherpa-onnx (en-tête binaire + échantillons float32, réponse
//! JSON).
//!
//! Modèles visés : NVIDIA Parakeet-TDT-0.6B-v3 (transducteur NeMo) et NVIDIA
//! Canary-180M-flash, publiés sous forme d'archives `.tar.bz2` dans la release
//! `asr-models` de sherpa-onnx. `models::download` récupère l'archive telle
//! quelle ; elle est décompressée ici au premier démarrage.
//!
//! Le runtime (binaires précompilés sherpa-onnx) n'est pas embarqué dans le
//! bundle : `ensure_runtime` le télécharge à la demande depuis la release
//! GitHub vers `<data_dir>/runtimes/sherpa-onnx/`.

use super::sidecar;
use super::{Engine, EngineKind, EngineState, EngineStatus, Transcript};
use crate::models::ModelSpec;
use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const KIND: EngineKind = EngineKind::SherpaOnnx;

/// Version des binaires précompilés sherpa-onnx utilisée par Murmure.
pub const RUNTIME_VERSION: &str = "v1.13.7";

/// Taille des trames binaires envoyées au serveur (identique au client Python
/// de référence de sherpa-onnx).
const CHUNK_BYTES: usize = 10_240;

/// Marqueur permettant de reconnaître notre sous-processus (`ps -o command=`).
const PROC_MARKER: &str = "sherpa-onnx-offline-websocket-server";

/// `<data_dir>/runtimes/sherpa-onnx`
pub fn runtime_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("runtimes").join(KIND.id())
}

/// Archive de binaires précompilés adaptée à la plateforme courante.
///
/// On prend les variantes « shared » : les variantes « static » existent mais
/// pèsent plus de 200 Mo (contre ~20 Mo), et le rpath `@loader_path/../lib`
/// (macOS) / `$ORIGIN/../lib` (Linux) des binaires suffit tant que `bin/` et
/// `lib/` restent côte à côte.
pub fn runtime_asset() -> Option<&'static str> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("sherpa-onnx-v1.13.7-osx-arm64-shared.tar.bz2")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("sherpa-onnx-v1.13.7-osx-x64-shared.tar.bz2")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        // MT = runtime C statique : pas de redistribuable MSVC à installer.
        Some("sherpa-onnx-v1.13.7-win-x64-shared-MT-Release.tar.bz2")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("sherpa-onnx-v1.13.7-linux-x64-shared.tar.bz2")
    } else {
        None
    }
}

/// Cherche `sherpa-onnx-offline-websocket-server`. Voir [`super::find_binary`]
/// pour l'ordre des emplacements inspectés.
pub fn find_runtime(override_path: Option<&str>) -> Option<PathBuf> {
    super::find_binary(KIND, override_path, None)
}

/// Comme [`find_runtime`], mais en inspectant aussi
/// `<data_dir>/runtimes/sherpa-onnx/bin`.
pub fn find_runtime_in(data_dir: Option<&Path>, override_path: Option<&str>) -> Option<PathBuf> {
    super::find_binary(KIND, override_path, data_dir)
}

/// Télécharge et installe les binaires sherpa-onnx dans
/// `<data_dir>/runtimes/sherpa-onnx/` s'ils manquent. `on_progress` reçoit
/// `(octets reçus, octets attendus)`.
pub async fn ensure_runtime(data_dir: &Path, on_progress: impl Fn(u64, u64)) -> Result<PathBuf> {
    if let Some(p) = find_runtime_in(Some(data_dir), None) {
        return Ok(p);
    }
    let asset = runtime_asset().ok_or_else(|| {
        anyhow!("sherpa-onnx ne fournit pas de binaires précompilés pour cette plateforme")
    })?;
    let url =
        format!("https://github.com/k2-fsa/sherpa-onnx/releases/download/{RUNTIME_VERSION}/{asset}");

    let root = runtime_dir(data_dir);
    let tmp_dir = root.join(".download");
    tokio::fs::create_dir_all(&tmp_dir)
        .await
        .context("création du dossier du runtime")?;
    let archive = tmp_dir.join(asset);

    let client = reqwest::Client::builder()
        .user_agent("murmure/0.1")
        .build()
        .context("client HTTP")?;
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("téléchargement de {asset}"))?
        .error_for_status()
        .with_context(|| format!("téléchargement de {asset}"))?;
    let total = resp.content_length().unwrap_or(0);

    {
        use futures_util::StreamExt;
        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::File::create(&archive)
            .await
            .context("écriture de l'archive du runtime")?;
        let mut got: u64 = 0;
        let mut last = Instant::now();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("téléchargement du runtime interrompu")?;
            file.write_all(&chunk).await?;
            got += chunk.len() as u64;
            if last.elapsed().as_millis() > 150 {
                on_progress(got, total);
                last = Instant::now();
            }
        }
        file.flush().await?;
        on_progress(got, total);
    }

    let dest = root.clone();
    let archive_c = archive.clone();
    tokio::task::spawn_blocking(move || extract_tar_bz2_strip1(&archive_c, &dest))
        .await
        .map_err(|e| anyhow!("décompression du runtime : {e}"))??;
    let _ = tokio::fs::remove_file(&archive).await;
    let _ = tokio::fs::remove_dir(&tmp_dir).await;

    make_executable(&root.join("bin"));

    find_runtime_in(Some(data_dir), None)
        .ok_or_else(|| anyhow!("{PROC_MARKER} absent après décompression du runtime"))
}

/// Décompresse une archive `.tar.bz2` dans `dest` en retirant le dossier racine
/// de l'archive (`--strip-components=1`).
pub fn extract_tar_bz2_strip1(archive: &Path, dest: &Path) -> Result<()> {
    let file = std::fs::File::open(archive)
        .with_context(|| format!("ouverture de {}", archive.display()))?;
    let decoder = bzip2::read::BzDecoder::new(std::io::BufReader::new(file));
    let mut tar = tar::Archive::new(decoder);
    tar.set_overwrite(true);
    std::fs::create_dir_all(dest)?;

    for entry in tar.entries().context("archive tar illisible")? {
        let mut entry = entry.context("entrée tar illisible")?;
        let path = entry.path().context("chemin tar invalide")?.into_owned();
        let mut comps = path.components();
        comps.next(); // dossier racine de l'archive
        let rel: PathBuf = comps.as_path().to_path_buf();
        if rel.as_os_str().is_empty() {
            continue;
        }
        // Sécurité : refuser tout chemin qui sortirait de `dest`.
        if rel.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::RootDir
            )
        }) {
            bail!("chemin suspect dans l'archive : {}", rel.display());
        }
        let out = dest.join(&rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        entry
            .unpack(&out)
            .with_context(|| format!("extraction de {}", rel.display()))?;
    }
    Ok(())
}

/// Rend exécutables tous les fichiers d'un dossier (Unix uniquement).
fn make_executable(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_file() {
                continue;
            }
            if let Ok(md) = std::fs::metadata(&p) {
                let mut perm = md.permissions();
                let mode = perm.mode();
                perm.set_mode(mode | 0o755);
                let _ = std::fs::set_permissions(&p, perm);
            }
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
}

// ---------------------------------------------------------------------------
// Modèles
// ---------------------------------------------------------------------------

/// Famille de modèle : les deux n'utilisent pas les mêmes options de ligne de
/// commande. Déduite de l'identifiant du modèle, faute de champ dédié dans
/// `ModelSpec`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    /// Transducteur NeMo (Parakeet TDT) : encoder + decoder + joiner.
    NemoTransducer,
    /// Canary : `--canary-encoder` / `--canary-decoder`.
    Canary,
}

/// Déduit la famille depuis `spec.id`, puis à défaut depuis les noms de
/// fichiers déclarés au catalogue.
pub fn flavor_of(spec: &ModelSpec) -> Flavor {
    let mut hay = format!("{} {}", spec.id, spec.main_file).to_lowercase();
    for f in &spec.files {
        hay.push(' ');
        hay.push_str(&f.name.to_lowercase());
    }
    if hay.contains("canary") {
        Flavor::Canary
    } else {
        Flavor::NemoTransducer
    }
}

/// Langue de travail de Canary. Ce modèle ne détecte pas la langue : il faut
/// lui donner une langue source et une langue cible, et il traduit vers
/// l'anglais si on ne dit rien. On se cale donc sur le français par défaut
/// (`src == tgt` = transcription sans traduction), surchargeable par
/// `MURMURE_CANARY_LANG`.
fn canary_lang() -> String {
    const OK: [&str; 4] = ["en", "de", "es", "fr"];
    std::env::var("MURMURE_CANARY_LANG")
        .ok()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| OK.contains(&s.as_str()))
        .unwrap_or_else(|| "fr".to_string())
}

/// Fichiers ONNX résolus dans le dossier du modèle après décompression.
#[derive(Debug, Clone)]
struct ModelFiles {
    tokens: PathBuf,
    encoder: PathBuf,
    decoder: PathBuf,
    joiner: Option<PathBuf>,
}

/// Choisit le meilleur `<prefix>*.onnx` d'un dossier : on privilégie les
/// variantes quantifiées (les archives `-int8` contiennent aussi les poids
/// fp32, bien plus lourds à charger).
fn pick_onnx(dir: &Path, prefix: &str) -> Option<PathBuf> {
    let mut best: Option<(u8, usize, PathBuf)> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let name = p.file_name()?.to_string_lossy().to_lowercase();
        if !name.starts_with(prefix) || !name.ends_with(".onnx") {
            continue;
        }
        let rank = if name.contains("int8") {
            0
        } else if name.contains("fp16") {
            1
        } else {
            2
        };
        let better = match &best {
            None => true,
            Some((r, len, _)) => rank < *r || (rank == *r && name.len() < *len),
        };
        if better {
            best = Some((rank, name.len(), p));
        }
    }
    best.map(|(_, _, p)| p)
}

fn locate_model(dir: &Path, flavor: Flavor) -> Result<ModelFiles> {
    let tokens = dir.join("tokens.txt");
    if !tokens.is_file() {
        bail!("tokens.txt introuvable dans {}", dir.display());
    }
    let encoder = pick_onnx(dir, "encoder")
        .ok_or_else(|| anyhow!("encoder*.onnx introuvable dans {}", dir.display()))?;
    let decoder = pick_onnx(dir, "decoder")
        .ok_or_else(|| anyhow!("decoder*.onnx introuvable dans {}", dir.display()))?;
    let joiner = pick_onnx(dir, "joiner");
    if flavor == Flavor::NemoTransducer && joiner.is_none() {
        bail!("joiner*.onnx introuvable dans {}", dir.display());
    }
    Ok(ModelFiles {
        tokens,
        encoder,
        decoder,
        joiner,
    })
}

/// Décompresse l'archive du modèle si ce n'est pas déjà fait. Un marqueur
/// `.extracted` évite de recommencer à chaque démarrage.
fn ensure_model_extracted(spec: &ModelSpec, dir: &Path) -> Result<()> {
    let marker = dir.join(".extracted");
    if marker.is_file() {
        return Ok(());
    }
    let archive = dir.join(&spec.main_file);
    if !archive.is_file() {
        bail!("archive du modèle absente : {}", archive.display());
    }
    log::info!("sherpa-onnx : décompression de {}", archive.display());
    extract_tar_bz2_strip1(&archive, dir)?;
    std::fs::write(&marker, &spec.main_file)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Audio
// ---------------------------------------------------------------------------

/// Convertit un WAV PCM (mono ou multicanal) en échantillons f32 dans [-1, 1].
pub fn wav_to_f32(wav: &[u8]) -> Result<(Vec<f32>, u32)> {
    let mut reader = hound::WavReader::new(std::io::Cursor::new(wav)).context("WAV illisible")?;
    let spec = reader.spec();
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("échantillons WAV illisibles")?,
        hound::SampleFormat::Int => match spec.bits_per_sample {
            16 => reader
                .samples::<i16>()
                .map(|s| s.map(|v| v as f32 / 32768.0))
                .collect::<std::result::Result<Vec<_>, _>>()
                .context("échantillons WAV illisibles")?,
            32 => reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / 2_147_483_648.0))
                .collect::<std::result::Result<Vec<_>, _>>()
                .context("échantillons WAV illisibles")?,
            n => bail!("WAV {n} bits non pris en charge"),
        },
    };
    let ch = spec.channels.max(1) as usize;
    let mono = if ch > 1 {
        raw.chunks(ch)
            .map(|c| c.iter().sum::<f32>() / ch as f32)
            .collect()
    } else {
        raw
    };
    Ok((mono, spec.sample_rate))
}

/// Trame attendue par le serveur : fréquence d'échantillonnage (i32 LE), taille
/// utile en octets (i32 LE), puis les échantillons f32 LE.
fn build_payload(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(8 + samples.len() * 4);
    buf.extend_from_slice(&(sample_rate as i32).to_le_bytes());
    buf.extend_from_slice(&((samples.len() * 4) as i32).to_le_bytes());
    for s in samples {
        buf.extend_from_slice(&s.to_le_bytes());
    }
    buf
}

/// Réponse du serveur : `OfflineRecognitionResult::AsJsonString()`, un objet
/// contenant au moins `text`, parfois `lang` (Canary, Whisper…).
pub fn parse_result(raw: &str) -> (Option<String>, String) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        return (None, raw.trim().to_string());
    };
    let text = v["text"].as_str().unwrap_or("").trim().to_string();
    let language = v["lang"]
        .as_str()
        .map(|s| s.trim().trim_matches(|c| c == '<' || c == '>' || c == '|').to_string())
        .filter(|s| !s.is_empty());
    (language, text)
}

// ---------------------------------------------------------------------------
// Moteur
// ---------------------------------------------------------------------------

struct Running {
    child: Child,
    port: u16,
}

pub struct SherpaEngine {
    data_dir: PathBuf,
    running: Mutex<Option<Running>>,
    status: Mutex<EngineStatus>,
}

impl SherpaEngine {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            running: Mutex::new(None),
            status: Mutex::new(EngineStatus::stopped()),
        }
    }

    async fn set_status(
        &self,
        state: EngineState,
        message: impl Into<String>,
        model_id: Option<String>,
        port: Option<u16>,
        runtime: Option<String>,
    ) {
        let mut s = self.status.lock().await;
        *s = EngineStatus {
            state,
            message: message.into(),
            model_id,
            port,
            runtime_path: runtime,
        };
    }
}

/// Tue un serveur sherpa-onnx orphelin laissé par une précédente instance.
pub fn kill_stale(_data_dir: &Path) {
    sidecar::kill_stale(KIND, PROC_MARKER);
}

/// Fichier de log applicatif du serveur (option `--log-file`, dont la valeur
/// par défaut `./log.txt` écrirait dans le répertoire courant).
fn server_log_path() -> PathBuf {
    std::env::temp_dir().join("murmure-sherpa-onnx-server.log")
}

fn log_tail() -> String {
    let mut parts: Vec<String> = Vec::new();
    for p in [sidecar::log_path(KIND), server_log_path()] {
        let t = sidecar::log_tail(&p, 6);
        if !t.trim().is_empty() {
            parts.push(t.trim().to_string());
        }
    }
    parts.join("\n")
}

/// Ajoute `<runtime>/../lib` aux chemins de recherche des bibliothèques
/// dynamiques. Les binaires « shared » embarquent déjà un rpath relatif, mais
/// sur Windows seul `PATH` compte.
fn apply_lib_path(cmd: &mut Command, runtime: &Path) {
    let Some(bin_dir) = runtime.parent() else { return };
    let lib_dir = bin_dir.join("..").join("lib");
    let lib_dir = lib_dir.canonicalize().unwrap_or(lib_dir);
    let var = if cfg!(target_os = "macos") {
        "DYLD_LIBRARY_PATH"
    } else if cfg!(windows) {
        "PATH"
    } else {
        "LD_LIBRARY_PATH"
    };
    let mut paths: Vec<PathBuf> = vec![lib_dir, bin_dir.to_path_buf()];
    if let Ok(existing) = std::env::var(var) {
        paths.extend(std::env::split_paths(&existing));
    }
    if let Ok(joined) = std::env::join_paths(paths) {
        cmd.env(var, joined);
    }
}

/// Le serveur n'ouvre son port qu'une fois le modèle chargé : une connexion TCP
/// acceptée vaut donc signal « prêt ».
async fn port_open(port: u16) -> bool {
    tokio::time::timeout(
        Duration::from_millis(500),
        tokio::net::TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

#[async_trait::async_trait]
impl Engine for SherpaEngine {
    fn kind(&self) -> EngineKind {
        KIND
    }

    async fn start(&self, spec: &ModelSpec, dir: &Path, runtime_override: Option<&str>) -> Result<()> {
        self.stop().await;

        let runtime = match find_runtime_in(Some(&self.data_dir), runtime_override) {
            Some(r) => r,
            None => {
                self.set_status(
                    EngineState::Error,
                    "Runtime sherpa-onnx introuvable. Lancez son installation depuis les réglages, ou indiquez le chemin de sherpa-onnx-offline-websocket-server.",
                    Some(spec.id.clone()),
                    None,
                    None,
                )
                .await;
                return Err(anyhow!("runtime sherpa-onnx introuvable"));
            }
        };
        let runtime_str = runtime.to_string_lossy().to_string();
        let flavor = flavor_of(spec);

        // Décompression de l'archive du modèle au premier démarrage.
        self.set_status(
            EngineState::Starting,
            "Décompression du modèle…",
            Some(spec.id.clone()),
            None,
            Some(runtime_str.clone()),
        )
        .await;
        {
            let spec_c = spec.clone();
            let dir_c = dir.to_path_buf();
            let res = tokio::task::spawn_blocking(move || ensure_model_extracted(&spec_c, &dir_c))
                .await
                .map_err(|e| anyhow!("{e}"))
                .and_then(|r| r);
            if let Err(e) = res {
                self.set_status(
                    EngineState::Error,
                    format!("Décompression du modèle impossible : {e}"),
                    Some(spec.id.clone()),
                    None,
                    Some(runtime_str),
                )
                .await;
                return Err(e);
            }
        }

        let files = match locate_model(dir, flavor) {
            Ok(f) => f,
            Err(e) => {
                self.set_status(
                    EngineState::Error,
                    format!("Modèle incomplet : {e}"),
                    Some(spec.id.clone()),
                    None,
                    Some(runtime_str),
                )
                .await;
                return Err(e);
            }
        };

        let port = sidecar::free_port()?;
        self.set_status(
            EngineState::Starting,
            "Chargement du modèle…",
            Some(spec.id.clone()),
            Some(port),
            Some(runtime_str.clone()),
        )
        .await;

        let _ = std::fs::remove_file(server_log_path());
        let log = std::fs::File::create(sidecar::log_path(KIND)).context("log sherpa-onnx")?;
        let log_err = log.try_clone()?;

        let mut cmd = Command::new(&runtime);
        cmd.arg(format!("--port={port}"))
            .arg(format!("--tokens={}", files.tokens.display()))
            .arg("--num-threads=4")
            .arg("--num-work-threads=2")
            .arg("--num-io-threads=1")
            .arg("--max-batch-size=1")
            .arg("--max-utterance-length=600")
            .arg("--decoding-method=greedy_search")
            .arg(format!("--log-file={}", server_log_path().display()));
        match flavor {
            Flavor::NemoTransducer => {
                cmd.arg(format!("--encoder={}", files.encoder.display()))
                    .arg(format!("--decoder={}", files.decoder.display()))
                    .arg(format!(
                        "--joiner={}",
                        files.joiner.as_ref().expect("joiner vérifié").display()
                    ))
                    // Évite un double chargement du modèle au démarrage.
                    .arg("--model-type=nemo_transducer");
            }
            Flavor::Canary => {
                // Canary est un modèle de traduction : sans langue explicite il
                // rend systématiquement de l'anglais. `src == tgt` force la
                // transcription dans la langue d'origine.
                let lang = canary_lang();
                cmd.arg(format!("--canary-encoder={}", files.encoder.display()))
                    .arg(format!("--canary-decoder={}", files.decoder.display()))
                    .arg(format!("--canary-src-lang={lang}"))
                    .arg(format!("--canary-tgt-lang={lang}"))
                    .arg("--canary-use-pnc=true");
            }
        }
        apply_lib_path(&mut cmd, &runtime);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err));
        sidecar::hide_console(&mut cmd);

        let child = cmd
            .spawn()
            .with_context(|| format!("lancement de {}", runtime.display()))?;
        let _ = std::fs::write(sidecar::pid_path(KIND), child.id().to_string());
        {
            let mut r = self.running.lock().await;
            *r = Some(Running { child, port });
        }

        let deadline = Instant::now() + Duration::from_secs(300);
        loop {
            {
                let mut r = self.running.lock().await;
                match r.as_mut() {
                    Some(run) => {
                        if let Ok(Some(code)) = run.child.try_wait() {
                            *r = None;
                            drop(r);
                            let msg =
                                format!("Le serveur sherpa-onnx s'est arrêté ({code}). {}", log_tail());
                            self.set_status(
                                EngineState::Error,
                                msg.clone(),
                                Some(spec.id.clone()),
                                None,
                                Some(runtime_str),
                            )
                            .await;
                            return Err(anyhow!(msg));
                        }
                    }
                    None => return Err(anyhow!("démarrage interrompu")),
                }
            }
            if port_open(port).await {
                break;
            }
            if Instant::now() > deadline {
                self.stop().await;
                self.set_status(
                    EngineState::Error,
                    format!("Le moteur n'a pas répondu à temps. {}", log_tail()),
                    Some(spec.id.clone()),
                    None,
                    Some(runtime_str),
                )
                .await;
                return Err(anyhow!("timeout démarrage sherpa-onnx"));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }

        self.set_status(
            EngineState::Ready,
            "Prêt",
            Some(spec.id.clone()),
            Some(port),
            Some(runtime_str),
        )
        .await;
        Ok(())
    }

    async fn stop(&self) {
        {
            let mut r = self.running.lock().await;
            if let Some(mut run) = r.take() {
                let _ = run.child.kill();
                let _ = run.child.wait();
                let _ = std::fs::remove_file(sidecar::pid_path(KIND));
            }
        }
        let mut s = self.status.lock().await;
        if s.state != EngineState::Error {
            *s = EngineStatus::stopped();
        }
    }

    async fn status(&self) -> EngineStatus {
        self.status.lock().await.clone()
    }

    async fn transcribe(&self, wav: &[u8]) -> Result<Transcript> {
        let port = {
            let r = self.running.lock().await;
            r.as_ref()
                .map(|x| x.port)
                .ok_or_else(|| anyhow!("moteur non démarré"))?
        };
        let (samples, sample_rate) = wav_to_f32(wav)?;
        if samples.is_empty() {
            return Ok(Transcript {
                text: String::new(),
                language: None,
                duration_ms: 0,
            });
        }
        let t0 = Instant::now();
        let raw = tokio::time::timeout(
            Duration::from_secs(600),
            decode(port, &samples, sample_rate),
        )
        .await
        .map_err(|_| anyhow!("le moteur n'a pas répondu (délai dépassé)"))??;
        let (language, text) = parse_result(&raw);
        Ok(Transcript {
            text,
            language,
            duration_ms: t0.elapsed().as_millis(),
        })
    }

    /// Ces modèles ne décodent pas jeton par jeton : on émet une seule fois le
    /// texte final comme « partiel ».
    async fn transcribe_stream(
        &self,
        wav: &[u8],
        on_partial: &(dyn Fn(String) + Send + Sync),
    ) -> Result<Transcript> {
        let t = self.transcribe(wav).await?;
        on_partial(t.text.clone());
        Ok(t)
    }
}

/// Un aller-retour websocket avec le serveur « offline » de sherpa-onnx.
async fn decode(port: u16, samples: &[f32], sample_rate: u32) -> Result<String> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let url = format!("ws://127.0.0.1:{port}/");
    let (mut ws, _) = tokio_tungstenite::connect_async(url.as_str())
        .await
        .context("connexion au moteur sherpa-onnx")?;

    let buf = build_payload(samples, sample_rate);
    for chunk in buf.chunks(CHUNK_BYTES) {
        ws.send(Message::Binary(chunk.to_vec().into()))
            .await
            .context("envoi de l'audio au moteur")?;
    }

    let mut result: Option<String> = None;
    while let Some(msg) = ws.next().await {
        match msg.context("flux websocket interrompu")? {
            Message::Text(t) => {
                result = Some(t.to_string());
                break;
            }
            Message::Close(frame) => {
                let reason = frame.map(|f| f.reason.to_string()).unwrap_or_default();
                if reason.is_empty() {
                    bail!("le moteur a fermé la connexion sans répondre");
                }
                bail!("le moteur a refusé la requête : {reason}");
            }
            _ => {}
        }
    }

    // Signale au serveur qu'il peut fermer la connexion.
    let _ = ws.send(Message::Text("Done".into())).await;
    let _ = ws.close(None).await;

    result.ok_or_else(|| anyhow!("aucune réponse du moteur"))
}

impl Drop for SherpaEngine {
    fn drop(&mut self) {
        if let Ok(mut r) = self.running.try_lock() {
            if let Some(mut run) = r.take() {
                let _ = run.child.kill();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_header() {
        let p = build_payload(&[1.0, -1.0], 16000);
        assert_eq!(&p[0..4], &16000i32.to_le_bytes());
        assert_eq!(&p[4..8], &8i32.to_le_bytes());
        assert_eq!(p.len(), 16);
    }

    #[test]
    fn result_parsing() {
        let (lang, text) = parse_result(r#"{"text":"bonjour","lang":"<|fr|>","tokens":[]}"#);
        assert_eq!(text, "bonjour");
        assert_eq!(lang.as_deref(), Some("fr"));
        let (lang, text) = parse_result(r#"{"text":" salut ","lang":""}"#);
        assert_eq!(text, "salut");
        assert!(lang.is_none());
    }
}
