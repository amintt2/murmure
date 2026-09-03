//! Capture micro via cpal, sur un thread dédié (les flux cpal ne sont pas `Send`).
//! Le tampon est partagé pour permettre la transcription incrémentale pendant
//! l'enregistrement. Sortie : WAV mono 16 kHz PCM 16 bits en mémoire.

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const TARGET_RATE: u32 = 16_000;

pub struct Recorder {
    stop_tx: Mutex<Option<mpsc::Sender<()>>>,
    done_rx: Mutex<Option<mpsc::Receiver<()>>>,
    buffer: Arc<Mutex<Vec<f32>>>,
    sample_rate: u32,
    started: Instant,
}

impl Recorder {
    /// Démarre la capture. `on_level` reçoit un niveau (0..1) ~20 fois/s.
    pub fn start(on_level: impl Fn(f32) + Send + Sync + 'static) -> Result<Self> {
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<u32>>();
        let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let buf2 = buffer.clone();

        std::thread::Builder::new()
            .name("murmure-audio".into())
            .spawn(move || {
                capture_thread(buf2, stop_rx, ready_tx, on_level);
                let _ = done_tx.send(());
            })
            .context("impossible de lancer le thread audio")?;

        let sample_rate = ready_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| anyhow!("le micro ne répond pas"))??;

        Ok(Self {
            stop_tx: Mutex::new(Some(stop_tx)),
            done_rx: Mutex::new(Some(done_rx)),
            buffer,
            sample_rate,
            started: Instant::now(),
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Nombre d'échantillons capturés jusqu'ici.
    pub fn len(&self) -> usize {
        self.buffer.lock().unwrap().len()
    }

    /// Copie des échantillons à partir de l'index `from`.
    pub fn snapshot(&self, from: usize) -> Vec<f32> {
        let b = self.buffer.lock().unwrap();
        if from >= b.len() {
            Vec::new()
        } else {
            b[from..].to_vec()
        }
    }

    /// Arrête la capture (idempotent). Le tampon reste lisible.
    pub fn stop(&self) -> Result<()> {
        if let Some(tx) = self.stop_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }
        if let Some(rx) = self.done_rx.lock().unwrap().take() {
            rx.recv_timeout(Duration::from_secs(5))
                .map_err(|_| anyhow!("l'arrêt de la capture a expiré"))?;
        }
        Ok(())
    }
}

fn capture_thread(
    buffer: Arc<Mutex<Vec<f32>>>,
    stop_rx: mpsc::Receiver<()>,
    ready_tx: mpsc::Sender<Result<u32>>,
    on_level: impl Fn(f32) + Send + Sync + 'static,
) {
    let host = cpal::default_host();
    let Some(device) = host.default_input_device() else {
        let _ = ready_tx.send(Err(anyhow!("aucun micro détecté")));
        return;
    };
    let supported = match device.default_input_config() {
        Ok(c) => c,
        Err(e) => {
            let _ = ready_tx.send(Err(anyhow!("micro indisponible : {e}")));
            return;
        }
    };
    let sample_rate = supported.sample_rate().0;
    let channels = supported.channels() as usize;
    let config: cpal::StreamConfig = supported.config();
    buffer.lock().unwrap().reserve(sample_rate as usize * 60);

    let last_level = Arc::new(AtomicU64::new(0));
    let on_level = Arc::new(on_level);
    let err_fn = |e| log::error!("erreur flux audio : {e}");

    macro_rules! build {
        ($t:ty) => {{
            let buffer = buffer.clone();
            let last_level = last_level.clone();
            let on_level = on_level.clone();
            device.build_input_stream(
                &config,
                move |data: &[$t], _| {
                    let mut buf = buffer.lock().unwrap();
                    let mut sum_sq = 0.0f32;
                    let mut n = 0usize;
                    for frame in data.chunks(channels) {
                        let mut acc = 0.0f32;
                        for s in frame {
                            acc += cpal::Sample::to_sample::<f32>(*s);
                        }
                        let mono = acc / channels as f32;
                        sum_sq += mono * mono;
                        n += 1;
                        buf.push(mono);
                    }
                    drop(buf);
                    if n > 0 {
                        let now = now_ms();
                        if now.saturating_sub(last_level.load(Ordering::Relaxed)) >= 50 {
                            last_level.store(now, Ordering::Relaxed);
                            let rms = (sum_sq / n as f32).sqrt();
                            on_level((rms * 4.0).min(1.0));
                        }
                    }
                },
                err_fn,
                None,
            )
        }};
    }

    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => build!(f32),
        cpal::SampleFormat::I16 => build!(i16),
        cpal::SampleFormat::U16 => build!(u16),
        cpal::SampleFormat::I32 => build!(i32),
        other => {
            let _ = ready_tx.send(Err(anyhow!("format audio non géré : {other:?}")));
            return;
        }
    };
    let stream = match stream {
        Ok(s) => s,
        Err(e) => {
            let _ = ready_tx.send(Err(anyhow!("ouverture du micro impossible : {e}")));
            return;
        }
    };
    if let Err(e) = stream.play() {
        let _ = ready_tx.send(Err(anyhow!("démarrage du micro impossible : {e}")));
        return;
    }
    let _ = ready_tx.send(Ok(sample_rate));

    let _ = stop_rx.recv();
    drop(stream);
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Encode des échantillons (taux `rate`) en WAV mono 16 kHz 16 bits.
pub fn to_wav_16k(samples: &[f32], rate: u32) -> Result<Vec<u8>> {
    let resampled = resample_linear(samples, rate, TARGET_RATE);
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec)?;
        for s in &resampled {
            writer.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
        }
        writer.finalize()?;
    }
    Ok(cursor.into_inner())
}

fn resample_linear(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = from as f64 / to as f64;
    let out_len = ((input.len() as f64) / ratio).floor() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = i as f64 * ratio;
        let idx = pos.floor() as usize;
        let frac = (pos - idx as f64) as f32;
        let a = input[idx.min(input.len() - 1)];
        let b = input[(idx + 1).min(input.len() - 1)];
        out.push(a + (b - a) * frac);
    }
    out
}

/// Énergie RMS par trame de 20 ms.
fn frame_rms(samples: &[f32], rate: u32) -> (Vec<f32>, usize) {
    let frame = (rate as usize / 50).max(1);
    let rms: Vec<f32> = samples
        .chunks(frame)
        .map(|c| (c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32).sqrt())
        .collect();
    (rms, frame)
}

/// Cherche une pause pour découper le flux : renvoie l'index (dans `samples`)
/// du milieu de la dernière plage silencieuse d'au moins `min_silence_ms`,
/// dont le centre se situe après `min_pos_s` secondes.
pub fn find_pause(samples: &[f32], rate: u32, min_silence_ms: u32, min_pos_s: f32) -> Option<usize> {
    let (rms, frame) = frame_rms(samples, rate);
    if rms.len() < 10 {
        return None;
    }
    let mut sorted = rms.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let floor = sorted[sorted.len() / 5];
    let peak = sorted[sorted.len() * 9 / 10];
    let threshold = (floor * 2.5).max(0.004).min(peak * 0.25);
    let min_frames = (min_silence_ms as usize / 20).max(1);
    let min_center = (min_pos_s * rate as f32) as usize;

    let mut best: Option<usize> = None;
    let mut run_start: Option<usize> = None;
    for i in 0..=rms.len() {
        let quiet = i < rms.len() && rms[i] < threshold;
        match (quiet, run_start) {
            (true, None) => run_start = Some(i),
            (false, Some(s)) => {
                if i - s >= min_frames {
                    let center = ((s + i) / 2) * frame;
                    if center >= min_center {
                        best = Some(center);
                    }
                }
                run_start = None;
            }
            _ => {}
        }
    }
    best
}

/// Index de la trame la moins énergique dans les `window_s` dernières secondes
/// (coupe de secours quand aucune pause nette n'est trouvée).
pub fn quietest_point(samples: &[f32], rate: u32, window_s: f32) -> usize {
    let (rms, frame) = frame_rms(samples, rate);
    let n = rms.len();
    let w = ((window_s * 50.0) as usize).min(n.saturating_sub(1)).max(1);
    let start = n - w;
    let (mut best_i, mut best_v) = (start, f32::MAX);
    for i in start..n {
        if rms[i] < best_v {
            best_v = rms[i];
            best_i = i;
        }
    }
    best_i * frame
}

/// Vrai si le segment contient de la parole plausible : au moins ~150 ms de
/// trames au-dessus d'un seuil d'énergie et une crête suffisante. Évite
/// d'envoyer du silence au modèle (qui hallucine du texte).
pub fn has_speech(samples: &[f32], rate: u32) -> bool {
    if samples.is_empty() {
        return false;
    }
    let (rms, _) = frame_rms(samples, rate);
    let loud = rms.iter().filter(|r| **r > 0.012).count();
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    loud >= 7 && peak > 0.04
}
