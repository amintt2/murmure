//! Moteur whisper.cpp : lance `whisper-server` et utilise son endpoint
//! `/inference` (multipart). Pas de streaming token par token : le texte
//! partiel est envoyé une fois, à la fin.

use super::sidecar::{free_port, hide_console, log_path, log_tail, pid_path};
use super::{find_binary, Engine, EngineKind, EngineState, EngineStatus, Transcript};
use crate::models::ModelSpec;
use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const KIND: EngineKind = EngineKind::WhisperCpp;

struct Running {
    child: Child,
    port: u16,
}

pub struct WhisperEngine {
    data_dir: PathBuf,
    running: Mutex<Option<Running>>,
    status: Mutex<EngineStatus>,
    client: reqwest::Client,
}

impl WhisperEngine {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            running: Mutex::new(None),
            status: Mutex::new(EngineStatus::stopped()),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(600))
                .build()
                .expect("client http"),
        }
    }

    async fn set_status(&self, state: EngineState, message: impl Into<String>, model_id: Option<String>, port: Option<u16>, runtime: Option<String>) {
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

#[async_trait::async_trait]
impl Engine for WhisperEngine {
    fn kind(&self) -> EngineKind {
        KIND
    }

    async fn start(&self, spec: &ModelSpec, dir: &Path, runtime_override: Option<&str>) -> Result<()> {
        self.stop().await;

        let Some(runtime) = find_binary(KIND, runtime_override, Some(&self.data_dir)) else {
            self.set_status(EngineState::Error, format!("whisper-server introuvable. {}", KIND.install_hint()), Some(spec.id.clone()), None, None).await;
            return Err(anyhow!("whisper-server introuvable"));
        };
        let runtime_str = runtime.to_string_lossy().to_string();
        let model = dir.join(&spec.main_file);
        if !model.is_file() {
            self.set_status(EngineState::Error, "Modèle non téléchargé", Some(spec.id.clone()), None, Some(runtime_str)).await;
            return Err(anyhow!("fichier modèle absent : {}", model.display()));
        }

        let port = free_port()?;
        self.set_status(EngineState::Starting, "Chargement du modèle…", Some(spec.id.clone()), Some(port), Some(runtime_str.clone())).await;

        let log = std::fs::File::create(log_path(KIND)).context("log whisper-server")?;
        let log_err = log.try_clone()?;
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 8);

        let mut cmd = Command::new(&runtime);
        cmd.arg("-m")
            .arg(&model)
            .args(["--host", "127.0.0.1", "--port", &port.to_string()])
            .args(["-t", &threads.to_string()])
            .args(["-l", "auto"])
            .arg("-nt")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err));
        hide_console(&mut cmd);

        let child = cmd.spawn().with_context(|| format!("lancement de {}", runtime.display()))?;
        let _ = std::fs::write(pid_path(KIND), child.id().to_string());
        {
            let mut r = self.running.lock().await;
            *r = Some(Running { child, port });
        }

        let url = format!("http://127.0.0.1:{port}/health");
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            {
                let mut r = self.running.lock().await;
                if let Some(run) = r.as_mut() {
                    if let Ok(Some(code)) = run.child.try_wait() {
                        *r = None;
                        let msg = format!("whisper-server s'est arrêté ({code}). {}", log_tail(&log_path(KIND), 8));
                        self.set_status(EngineState::Error, msg.clone(), Some(spec.id.clone()), None, Some(runtime_str)).await;
                        return Err(anyhow!(msg));
                    }
                } else {
                    return Err(anyhow!("démarrage interrompu"));
                }
            }
            if let Ok(resp) = self.client.get(&url).timeout(Duration::from_secs(2)).send().await {
                if resp.status().is_success() {
                    break;
                }
            }
            if Instant::now() > deadline {
                self.stop().await;
                self.set_status(EngineState::Error, "Le moteur n'a pas répondu à temps", Some(spec.id.clone()), None, Some(runtime_str)).await;
                return Err(anyhow!("timeout démarrage whisper-server"));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }

        self.set_status(EngineState::Ready, "Prêt", Some(spec.id.clone()), Some(port), Some(runtime_str)).await;
        Ok(())
    }

    async fn stop(&self) {
        let mut r = self.running.lock().await;
        if let Some(mut run) = r.take() {
            let _ = run.child.kill();
            let _ = run.child.wait();
            let _ = std::fs::remove_file(pid_path(KIND));
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
            r.as_ref().map(|x| x.port).ok_or_else(|| anyhow!("moteur non démarré"))?
        };
        let part = reqwest::multipart::Part::bytes(wav.to_vec())
            .file_name("audio.wav")
            .mime_str("audio/wav")?;
        let form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("response_format", "json")
            .text("temperature", "0.0")
            .text("language", "auto")
            .text("no_speech_thold", "0.6");
        let t0 = Instant::now();
        let resp = self
            .client
            .post(format!("http://127.0.0.1:{port}/inference"))
            .multipart(form)
            .send()
            .await
            .context("requête au moteur")?;
        let status = resp.status();
        let json: serde_json::Value = resp.json().await.context("réponse du moteur illisible")?;
        if !status.is_success() {
            return Err(anyhow!("moteur : {}", json["error"].as_str().unwrap_or("erreur inconnue")));
        }
        let text = json["text"].as_str().unwrap_or("").trim().to_string();
        Ok(Transcript {
            text,
            language: None,
            duration_ms: t0.elapsed().as_millis(),
        })
    }

    async fn transcribe_stream(&self, wav: &[u8], on_partial: &(dyn Fn(String) + Send + Sync)) -> Result<Transcript> {
        let t = self.transcribe(wav).await?;
        on_partial(t.text.clone());
        Ok(t)
    }
}

impl Drop for WhisperEngine {
    fn drop(&mut self) {
        if let Ok(mut r) = self.running.try_lock() {
            if let Some(mut run) = r.take() {
                let _ = run.child.kill();
            }
        }
    }
}
