//! Moteur llama.cpp : lance `llama-server` en sous-processus sur un port local
//! et l'interroge via l'API OpenAI-compatible (`input_audio`).

use super::sidecar::{free_port, hide_console, log_path, log_tail, pid_path};
use super::{find_binary, Engine, EngineKind, EngineState, EngineStatus, Transcript};
use crate::models::{ModelSpec, OutputFormat};
use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const KIND: EngineKind = EngineKind::LlamaCpp;

struct Running {
    child: Child,
    port: u16,
    prompt: Option<String>,
    output: OutputFormat,
}

pub struct LlamaEngine {
    data_dir: PathBuf,
    running: Mutex<Option<Running>>,
    status: Mutex<EngineStatus>,
    client: reqwest::Client,
}

impl LlamaEngine {
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

    async fn request(&self, wav: &[u8], stream: bool) -> Result<(u16, OutputFormat, serde_json::Value)> {
        let (port, prompt, output) = {
            let r = self.running.lock().await;
            let x = r.as_ref().ok_or_else(|| anyhow!("moteur non démarré"))?;
            (x.port, x.prompt.clone(), x.output)
        };
        let b64 = base64::engine::general_purpose::STANDARD.encode(wav);
        let mut content = vec![serde_json::json!({
            "type": "input_audio",
            "input_audio": { "data": b64, "format": "wav" }
        })];
        if let Some(p) = prompt {
            content.push(serde_json::json!({ "type": "text", "text": p }));
        }
        let body = serde_json::json!({
            "model": "murmure",
            "temperature": 0.0,
            "max_tokens": 2048,
            "stream": stream,
            "messages": [{ "role": "user", "content": content }]
        });
        Ok((port, output, body))
    }
}

/// Texte affichable pendant le streaming : ce qui suit `<asr_text>`, sinon rien
/// (l'en-tête `language X` n'est pas montré).
pub fn partial_text(raw: &str, output: OutputFormat) -> String {
    if output == OutputFormat::Plain {
        return raw.trim_start().to_string();
    }
    match raw.find("<asr_text>") {
        Some(i) => raw[i + "<asr_text>".len()..].trim_start().to_string(),
        None if raw.trim_start().starts_with("language") || raw.trim().is_empty() => String::new(),
        None => raw.trim_start().to_string(),
    }
}

/// Sortie brute de Qwen3-ASR : `language French<asr_text>Bonjour…`
pub fn parse_asr_output(raw: &str) -> (Option<String>, String) {
    let re = regex::Regex::new(r"(?s)^\s*language\s+([^<\n]+?)\s*<asr_text>\s*(.*?)\s*$").unwrap();
    if let Some(c) = re.captures(raw) {
        return (Some(c[1].trim().to_string()), c[2].trim().to_string());
    }
    let cleaned = raw.replace("<asr_text>", "").trim().to_string();
    (None, cleaned)
}

fn finalize(raw: &str, output: OutputFormat) -> (Option<String>, String) {
    match output {
        OutputFormat::Qwen3Asr => parse_asr_output(raw),
        OutputFormat::Plain => (None, raw.trim().to_string()),
    }
}

#[async_trait::async_trait]
impl Engine for LlamaEngine {
    fn kind(&self) -> EngineKind {
        KIND
    }

    async fn start(&self, spec: &ModelSpec, dir: &Path, runtime_override: Option<&str>) -> Result<()> {
        self.stop().await;

        let Some(runtime) = find_binary(KIND, runtime_override, Some(&self.data_dir)) else {
            self.set_status(EngineState::Error, format!("llama-server introuvable. {}", KIND.install_hint()), Some(spec.id.clone()), None, None).await;
            return Err(anyhow!("llama-server introuvable"));
        };
        let runtime_str = runtime.to_string_lossy().to_string();

        let model = dir.join(&spec.main_file);
        if !model.is_file() {
            self.set_status(EngineState::Error, "Modèle non téléchargé", Some(spec.id.clone()), None, Some(runtime_str)).await;
            return Err(anyhow!("fichier modèle absent : {}", model.display()));
        }

        let port = free_port()?;
        self.set_status(EngineState::Starting, "Chargement du modèle…", Some(spec.id.clone()), Some(port), Some(runtime_str.clone())).await;

        let log = std::fs::File::create(log_path(KIND)).context("log llama-server")?;
        let log_err = log.try_clone()?;

        let mut cmd = Command::new(&runtime);
        cmd.arg("-m").arg(&model);
        if let Some(mm) = &spec.mmproj_file {
            cmd.arg("--mmproj").arg(dir.join(mm));
        }
        cmd.args(["--host", "127.0.0.1", "--port", &port.to_string()])
            .args(["--alias", "murmure"])
            .args(["-ngl", "99"])
            .args(["-c", "8192"])
            .args(["-np", "1"])
            .arg("--no-webui")
            .args(["--log-verbosity", "0"])
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err));
        hide_console(&mut cmd);

        let child = cmd.spawn().with_context(|| format!("lancement de {}", runtime.display()))?;
        let _ = std::fs::write(pid_path(KIND), child.id().to_string());
        {
            let mut r = self.running.lock().await;
            *r = Some(Running {
                child,
                port,
                prompt: spec.prompt.clone(),
                output: spec.output,
            });
        }

        let url = format!("http://127.0.0.1:{port}/health");
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            {
                let mut r = self.running.lock().await;
                if let Some(run) = r.as_mut() {
                    if let Ok(Some(code)) = run.child.try_wait() {
                        *r = None;
                        let msg = format!("llama-server s'est arrêté ({code}). {}", log_tail(&log_path(KIND), 8));
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
                return Err(anyhow!("timeout démarrage llama-server"));
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
        let (port, output, body) = self.request(wav, false).await?;
        let t0 = Instant::now();
        let resp = self
            .client
            .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
            .json(&body)
            .send()
            .await
            .context("requête au moteur")?;
        let status = resp.status();
        let json: serde_json::Value = resp.json().await.context("réponse du moteur illisible")?;
        if !status.is_success() {
            return Err(anyhow!("moteur : {}", json["error"]["message"].as_str().unwrap_or("erreur inconnue")));
        }
        let raw = json["choices"][0]["message"]["content"].as_str().unwrap_or("").to_string();
        let (language, text) = finalize(&raw, output);
        Ok(Transcript {
            text,
            language,
            duration_ms: t0.elapsed().as_millis(),
        })
    }

    async fn transcribe_stream(&self, wav: &[u8], on_partial: &(dyn Fn(String) + Send + Sync)) -> Result<Transcript> {
        use futures_util::StreamExt;
        let (port, output, body) = self.request(wav, true).await?;
        let t0 = Instant::now();
        let resp = self
            .client
            .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
            .json(&body)
            .send()
            .await
            .context("requête au moteur")?;
        if !resp.status().is_success() {
            let json: serde_json::Value = resp.json().await.unwrap_or_default();
            return Err(anyhow!("moteur : {}", json["error"]["message"].as_str().unwrap_or("erreur inconnue")));
        }
        let mut stream = resp.bytes_stream();
        let mut pending = String::new();
        let mut raw = String::new();
        'outer: while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("flux interrompu")?;
            pending.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(nl) = pending.find('\n') {
                let line = pending[..nl].trim().to_string();
                pending = pending[nl + 1..].to_string();
                let Some(data) = line.strip_prefix("data:") else { continue };
                let data = data.trim();
                if data == "[DONE]" {
                    break 'outer;
                }
                let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else { continue };
                if let Some(msg) = v["error"]["message"].as_str() {
                    return Err(anyhow!("moteur : {msg}"));
                }
                if let Some(tok) = v["choices"][0]["delta"]["content"].as_str() {
                    raw.push_str(tok);
                    on_partial(partial_text(&raw, output));
                }
            }
        }
        let (language, text) = finalize(&raw, output);
        Ok(Transcript {
            text,
            language,
            duration_ms: t0.elapsed().as_millis(),
        })
    }
}

impl Drop for LlamaEngine {
    fn drop(&mut self) {
        if let Ok(mut r) = self.running.try_lock() {
            if let Some(mut run) = r.take() {
                let _ = run.child.kill();
            }
        }
    }
}
