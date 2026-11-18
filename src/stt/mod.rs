//! Speech to text for videos (REQ-MEDIA-004, DD-018): Whisper `base` with
//! candle, in pure Rust, run in a child process (`lichtbild transcribe`)
//! under time and memory limits, like the keyframe decoder (DD-016).
//!
//! What lichtbild adds to candle's example, all measured on open media
//! (spikes/stt-bench/README.md):
//! - the language is chosen per 30 s window, optionally among a configured
//!   set, instead of once from the first window;
//! - windows without voice energy are skipped before decoding;
//! - the compression ratio is computed, so the temperature fallback
//!   catches repetition loops; what still repeats is dropped;
//! - each window resumes at the last complete timestamp, so words at the
//!   seams are decoded whole.

mod audio;
mod languages;
mod whisper;

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use candle::{Device, Tensor};
use candle_transformers::models::whisper as m;
use serde::{Deserialize, Serialize};
use tokenizers::Tokenizer;

pub use languages::LANGUAGES;
use whisper::{token_id, Decoder, Model};

/// The model files lichtbild accepts, pinned by SHA-256 (docs/BRIEF.md):
/// `openai/whisper-base` at revision `refs/pr/22`, and the q8_0 `tiny`
/// from `lmz/candle-whisper` (tests only).
pub const MODELS: &[(&str, &str)] = &[
    (
        "whisper-base",
        "07cadb9f25677c8d50df603e66a98fbd842cce45047139baeb16e6219a1e807b",
    ),
    (
        "whisper-tiny-q80",
        "edcc907db61aef092f1244dc1e53c55056b472be343e9bbb4dc12ebd4740392f",
    ),
];

/// Longest stretch of audio transcribed per video; the rest is left out
/// and the transcript marked partial.
pub const MAX_AUDIO_SECONDS: f64 = 30.0 * 60.0;

/// One timed piece of a transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Piece {
    pub start: f32,
    pub end: f32,
    /// Whisper language code, e.g. "de".
    pub lang: String,
    pub text: String,
}

/// What the child returns, and what the index stores per content hash.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    /// "ok", "no-speech" or "failed (<reason>)".
    pub state: String,
    pub model: String,
    pub audio_seconds: f32,
    pub partial: bool,
    pub pieces: Vec<Piece>,
}

/// Find and verify the model files in `dir`: config.json, tokenizer.json,
/// and model.safetensors or model.gguf with a pinned hash. Returns the
/// model's name.
pub fn check_model(dir: &Path) -> Result<&'static str> {
    for f in ["config.json", "tokenizer.json"] {
        if !dir.join(f).is_file() {
            bail!("whisper model dir {}: {f} missing", dir.display());
        }
    }
    let weights = weights_path(dir)?;
    let mut h = blake3_free_sha256(&weights)?;
    h.make_ascii_lowercase();
    MODELS
        .iter()
        .find(|(_, sum)| *sum == h)
        .map(|(name, _)| *name)
        .with_context(|| format!("{}: unknown model (sha256 {h})", weights.display()))
}

fn weights_path(dir: &Path) -> Result<PathBuf> {
    let gguf = dir.join("model.gguf");
    let st = dir.join("model.safetensors");
    match (gguf.is_file(), st.is_file()) {
        (true, _) => Ok(gguf),
        (false, true) => Ok(st),
        _ => bail!("{}: no model.safetensors or model.gguf", dir.display()),
    }
}

fn blake3_free_sha256(p: &Path) -> Result<String> {
    use sha2::Digest;
    let mut f = File::open(p).with_context(|| p.display().to_string())?;
    let mut h = sha2::Sha256::new();
    std::io::copy(&mut f, &mut h)?;
    Ok(format!("{:x}", h.finalize()))
}

/// A loaded model, ready to transcribe.
pub struct Engine {
    decoder: Decoder,
    config: m::Config,
    filters: Vec<f32>,
    device: Device,
    allowed: Vec<u32>,
    codes: Vec<(u32, &'static str)>,
    name: &'static str,
}

impl Engine {
    /// Load the model in `dir`. `languages` limits detection to these
    /// Whisper codes (e.g. ["de", "en"]); empty means all.
    pub fn load(dir: &Path, languages: &[String]) -> Result<Engine> {
        let name = check_model(dir)?;
        let device = Device::Cpu;
        let config: m::Config =
            serde_json::from_str(&std::fs::read_to_string(dir.join("config.json"))?)?;
        let tokenizer =
            Tokenizer::from_file(dir.join("tokenizer.json")).map_err(anyhow::Error::msg)?;
        if config.num_mel_bins != 80 {
            bail!("unexpected num_mel_bins {}", config.num_mel_bins);
        }
        let bytes = include_bytes!("melfilters.bytes");
        let filters: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let weights = weights_path(dir)?;
        let model = if weights.extension().is_some_and(|e| e == "gguf") {
            let vb = candle_transformers::quantized_var_builder::VarBuilder::from_gguf(
                &weights, &device,
            )?;
            Model::Quantized(m::quantized_model::Whisper::load(&vb, config.clone())?)
        } else {
            let data = std::fs::read(&weights)?;
            let vb = candle_nn::VarBuilder::from_buffered_safetensors(data, m::DTYPE, &device)?;
            Model::Normal(m::model::Whisper::load(&vb, config.clone())?)
        };
        let mut allowed = Vec::new();
        for l in languages {
            match token_id(&tokenizer, &format!("<|{l}|>")) {
                Ok(t) => allowed.push(t),
                Err(_) => bail!("unknown language code {l:?}"),
            }
        }
        let codes = LANGUAGES
            .iter()
            .filter_map(|(c, _)| {
                token_id(&tokenizer, &format!("<|{c}|>"))
                    .ok()
                    .map(|t| (t, *c))
            })
            .collect();
        // The first timestamp of a window may be at most 1 s in (OpenAI's
        // max_initial_timestamp): without it Whisper may jump to the end of
        // a window and skip 30 s of speech.
        let decoder = Decoder::new(model, tokenizer, 299_792_458, &device, None, true, Some(50))?;
        Ok(Engine {
            decoder,
            config,
            filters,
            device,
            allowed,
            codes,
            name,
        })
    }

    /// Transcribe 16 kHz mono samples.
    pub fn transcribe(&mut self, pcm: &[f32]) -> Result<Vec<Piece>> {
        let mel = m::audio::pcm_to_mel(&self.config, pcm, &self.filters);
        let bins = self.config.num_mel_bins;
        let frames = mel.len() / bins;
        let mel = Tensor::from_vec(mel, (1, bins, frames), &self.device)?;
        // pcm_to_mel pads; only frames that hold audio are decoded.
        let content = (pcm.len() / m::HOP_LENGTH).min(frames);
        let voiced = voiced_frames(pcm);
        let mut pieces: Vec<Piece> = Vec::new();
        let mut seek = 0usize;
        while seek < content {
            // Skip silence: start at the next voiced frame.
            match voiced[seek..content].iter().position(|&v| v) {
                Some(off) => seek += off,
                None => break,
            }
            let size = (content - seek).min(m::N_FRAMES);
            let window = mel.narrow(2, seek, size)?;
            let offset = (seek * m::HOP_LENGTH) as f32 / m::SAMPLE_RATE as f32;
            let features = self.decoder.features(&window)?;
            let lang = self.decoder.language(&features, &self.allowed)?;
            let dr = self.decoder.decode_with_fallback(&features, lang)?;
            let no_speech =
                dr.no_speech_prob > m::NO_SPEECH_THRESHOLD && dr.avg_logprob < m::LOGPROB_THRESHOLD;
            let looped = dr.compression_ratio > m::COMPRESSION_RATIO_THRESHOLD;
            let (timed, resume) = self.decoder.timed_text(&dr.tokens)?;
            if std::env::var_os("LICHTBILD_STT_DEBUG").is_some() {
                eprintln!(
                    "window {offset:.1}s size {:.1}s lang {lang} no_speech {:.2} logprob {:.2} cr {:.2} t {} looped {looped} pieces {} resume {resume:?} rep {:?}",
                    size as f32 * 0.01, dr.no_speech_prob, dr.avg_logprob, dr.compression_ratio, dr.temperature, timed.len(),
                    timed.iter().map(|p| repetitive(&p.2)).collect::<Vec<_>>()
                );
            }
            if !no_speech && !looped {
                let code = self
                    .codes
                    .iter()
                    .find(|(t, _)| *t == lang)
                    .map_or("?", |(_, c)| c);
                for (s, e, text) in timed {
                    if repetitive(&text) {
                        continue;
                    }
                    pieces.push(Piece {
                        start: offset + s,
                        end: offset + e.min(size as f32 * 0.01),
                        lang: code.to_string(),
                        text,
                    });
                }
            }
            // Advance to the last complete timestamp, at least one second.
            let step = match resume {
                Some(at) if at >= 1.0 && !no_speech && !looped => {
                    ((at * m::SAMPLE_RATE as f32) as usize / m::HOP_LENGTH).min(size)
                }
                _ => size,
            };
            seek += step.max(1);
        }
        Ok(drop_echoes(pieces))
    }

    pub fn name(&self) -> &'static str {
        self.name
    }
}

/// Per mel frame (10 ms): whether the 30 ms around it carry voice energy,
/// at least -45 dBFS and 12 dB above the quietest tenth of the clip.
fn voiced_frames(pcm: &[f32]) -> Vec<bool> {
    let hop = m::HOP_LENGTH;
    let n = pcm.len().div_ceil(hop).max(1);
    let db: Vec<f32> = (0..n)
        .map(|i| {
            let a = (i * hop).saturating_sub(hop);
            let b = ((i + 2) * hop).min(pcm.len());
            let s = &pcm[a.min(b)..b];
            let rms = (s.iter().map(|x| x * x).sum::<f32>() / s.len().max(1) as f32).sqrt();
            20.0 * rms.max(1e-9).log10()
        })
        .collect();
    let mut sorted = db.clone();
    sorted.sort_by(f32::total_cmp);
    let floor = sorted[sorted.len() / 10];
    let threshold = (floor + 12.0).max(-45.0);
    // Keep 0.3 s around voiced frames so word edges are not clipped.
    let raw: Vec<bool> = db.iter().map(|&d| d >= threshold).collect();
    let pad = 30;
    let mut out = vec![false; n];
    for (i, &v) in raw.iter().enumerate() {
        if v {
            for o in out
                .iter_mut()
                .take((i + pad + 1).min(n))
                .skip(i.saturating_sub(pad))
            {
                *o = true;
            }
        }
    }
    out
}

/// Lines Whisper writes over music, noise and silence because its training
/// data (subtitles) had them; lower case, without punctuation.
const STOCK_PHRASES: &[&str] = &[
    "music",
    "musik",
    "applause",
    "applaus",
    "untertitel im auftrag des zdf",
    "untertitel im auftrag des zdf 2017",
    "untertitelung des zdf",
    "untertitel der amaraorg community",
    "vielen dank fürs zuschauen",
    "danke fürs zuschauen",
    "thank you for watching",
    "thanks for watching",
    "copyright wdr 2021",
];

fn normalized(text: &str) -> String {
    text.split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Drop stock phrases, and an identical line from its third time on: a
/// line Whisper repeats across windows ("I was young." seven times over a
/// film's music) is an echo, not speech.
fn drop_echoes(pieces: Vec<Piece>) -> Vec<Piece> {
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    pieces
        .into_iter()
        .filter(|p| {
            let n = normalized(&p.text);
            if n.is_empty() || STOCK_PHRASES.contains(&n.as_str()) {
                return false;
            }
            let c = seen.entry(n).or_insert(0);
            *c += 1;
            *c <= 2
        })
        .collect()
}

/// A piece that is mostly one word or phrase repeated ("I'm I'm I'm",
/// "Sssss"): what Whisper makes of music and noise.
fn repetitive(text: &str) -> bool {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        return true;
    }
    // One letter stretched out.
    if words.len() == 1 {
        let w = &words[0];
        let first = w.chars().next().unwrap_or(' ');
        return w.chars().count() > 6 && w.chars().all(|c| c == first);
    }
    if words.len() < 6 {
        return false;
    }
    // A phrase of one to four words repeated at least three times that
    // covers more than half the piece.
    for n in 1..=4usize {
        let mut counts = std::collections::HashMap::new();
        for g in words.windows(n) {
            *counts.entry(g).or_insert(0usize) += 1;
        }
        let top = counts.values().max().copied().unwrap_or(0);
        if top >= 3 && top * n * 2 > words.len() {
            return true;
        }
    }
    // Whisper's own measure, the compression ratio, catches longer loops.
    whisper::compression_ratio(text) > m::COMPRESSION_RATIO_THRESHOLD
}

/// Resample to 16 kHz.
fn to_16k(pcm: Vec<f32>, rate: u32) -> Result<Vec<f32>> {
    if rate == m::SAMPLE_RATE as u32 {
        Ok(pcm)
    } else if rate == 0 {
        bail!("unknown sample rate")
    } else {
        audio::resample(&pcm, rate, m::SAMPLE_RATE as u32)
    }
}

/// Transcribe one media file in this process.
pub fn transcribe_file(engine: &mut Engine, f: File) -> Result<Transcript> {
    let (pcm, rate, partial) = audio::pcm_decode(f, MAX_AUDIO_SECONDS)?;
    let pcm = to_16k(pcm, rate)?;
    let audio_seconds = pcm.len() as f32 / m::SAMPLE_RATE as f32;
    let pieces = engine.transcribe(&pcm)?;
    Ok(Transcript {
        state: if pieces.is_empty() { "no-speech" } else { "ok" }.into(),
        model: engine.name().into(),
        audio_seconds,
        partial,
        pieces,
    })
}

/// How long a child may run for audio of this length: Whisper base runs
/// at 1.2 to 1.7 times real time on the Pi 5; allow 6 times and a minute.
pub fn time_budget(audio_seconds: f64) -> Duration {
    Duration::from_secs_f64(audio_seconds.min(MAX_AUDIO_SECONDS) * 6.0 + 60.0)
}

/// Run `exe transcribe` on one file: the path (already resolved inside its
/// root) and the model directory go as arguments, the transcript comes
/// back as JSON on stdout. Killed after `timeout`.
pub fn run_child(
    exe: &Path,
    file: &Path,
    model_dir: &Path,
    languages: &[String],
    timeout: Duration,
) -> Result<Transcript> {
    use std::process::{Command, Stdio};
    let mut child = Command::new(exe)
        .arg("transcribe")
        .arg(model_dir)
        .arg(file)
        .arg(languages.join(","))
        .env_clear()
        .env("RAYON_NUM_THREADS", "4")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("start transcribe")?;
    let mut stdout = child.stdout.take().context("stdout")?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let r = (&mut stdout).take(64 << 20).read_to_end(&mut out);
        r.map(|_| out)
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let out = reader.join().map_err(|_| anyhow::anyhow!("reader"))??;
    match status {
        None => bail!("timeout"),
        Some(s) if !s.success() => bail!("exit {}", s.code().unwrap_or(-1)),
        Some(_) => Ok(serde_json::from_slice(&out).context("child output")?),
    }
}

/// The `transcribe` child: `lichtbild transcribe <model-dir> <file>
/// <languages>`. Limits its own CPU time and, on Linux, its address space.
pub fn child_main(args: &[&str]) -> Result<()> {
    let [model_dir, file, langs] = args else {
        bail!("usage: transcribe <model-dir> <file> <languages>");
    };
    let languages: Vec<String> = langs
        .split(',')
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    let f = File::open(file)?;
    // CPU: the same budget the parent's wall clock allows, for 4 threads.
    limit_self(time_budget(MAX_AUDIO_SECONDS).as_secs() * 4);
    let mut engine = Engine::load(Path::new(model_dir), &languages)?;
    let t = transcribe_file(&mut engine, f)?;
    serde_json::to_writer(std::io::stdout().lock(), &t)?;
    Ok(())
}

fn limit_self(cpu_seconds: u64) {
    let set = |res, v: u64| {
        let lim = libc::rlimit {
            rlim_cur: v as libc::rlim_t,
            rlim_max: v as libc::rlim_t,
        };
        // SAFETY: setrlimit(2) on this process with a valid struct.
        unsafe { libc::setrlimit(res, &lim) };
    };
    set(libc::RLIMIT_CPU, cpu_seconds);
    // Whisper base needs about 600 MB resident; the source audio for 30
    // minutes at 48 kHz another 350 MB.
    #[cfg(target_os = "linux")]
    set(libc::RLIMIT_AS, 4 << 30);
}

/// What one transcription pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct PassReport {
    pub transcribed: u64,
    pub no_speech: u64,
    pub failed: u64,
    /// Left for the next pass because the deadline came.
    pub remaining: u64,
}

impl PassReport {
    pub fn to_text(&self) -> String {
        format!(
            "transcripts: {} with speech, {} without, {} failed, {} left for the next pass",
            self.transcribed, self.no_speech, self.failed, self.remaining
        )
    }
}

/// Transcribe the videos that have no transcript from the configured
/// model yet, one child at a time, until `deadline`. Each result, failures
/// included, is stored under the video's content hash, so a pass never
/// repeats work and an interrupted pass resumes.
pub fn run_pass(
    cfg: &crate::config::Config,
    idx: &crate::index::Index,
    exe: &Path,
    deadline: Option<Instant>,
) -> Result<PassReport> {
    let mut rep = PassReport::default();
    let Some(t) = &cfg.transcribe else {
        return Ok(rep);
    };
    let todo = idx.videos_to_transcribe(t.model)?;
    let total = todo.len() as u64;
    for (n, (_, rec)) in todo.into_iter().enumerate() {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            rep.remaining = total - n as u64;
            break;
        }
        let Some(root) = cfg.roots.iter().find(|r| r.label == rec.label) else {
            continue;
        };
        let rel = crate::index::rel_from_bytes(&rec.rel);
        // SSC-004: the stored path must still resolve inside its root.
        let Ok(path) = crate::source::resolve(&root.path, &rel) else {
            continue;
        };
        let seconds = rec
            .video
            .as_ref()
            .map_or(60.0, |v| v.duration_ms as f64 / 1000.0);
        let tr = run_child(exe, &path, &t.model_dir, &t.languages, time_budget(seconds))
            .unwrap_or_else(|e| Transcript {
                state: format!("failed ({})", short_reason(&e)),
                model: t.model.into(),
                audio_seconds: 0.0,
                partial: false,
                pieces: Vec::new(),
            });
        match tr.state.as_str() {
            "ok" => rep.transcribed += 1,
            "no-speech" => rep.no_speech += 1,
            _ => rep.failed += 1,
        }
        idx.set_transcript(&rec.hash, &tr)?;
    }
    Ok(rep)
}

/// A failure reason short and free of paths, for the index and the log.
fn short_reason(e: &anyhow::Error) -> String {
    let s = e.to_string();
    let s = s.split(':').next().unwrap_or("error").trim();
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == ' ' || *c == '-')
        .take(40)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repetition_is_recognised() {
        assert!(repetitive("I'm I'm I'm I'm I'm I'm I'm"));
        assert!(repetitive("Sssssssssssssssssss"));
        assert!(repetitive("..."));
        assert!(!repetitive("Heute ist ein schöner Tag im Garten."));
        assert!(!repetitive("Come on!"));
        assert!(repetitive(
            "Ich bin ein Erschweißer. Ich bin ein Erschweißer. Ich bin ein Erschweißer."
        ));
    }

    #[test]
    fn echoes_and_stock_phrases_are_dropped() {
        let p = |t: &str| Piece {
            start: 0.0,
            end: 1.0,
            lang: "de".into(),
            text: t.into(),
        };
        let kept: Vec<String> = drop_echoes(vec![
            p("Untertitel im Auftrag des ZDF, 2017"),
            p("[Musik]"),
            p("Komm!"),
            p("komm"),
            p("Komm!"),
            p("Heute backen wir Kuchen."),
        ])
        .into_iter()
        .map(|x| x.text)
        .collect();
        assert_eq!(kept, ["Komm!", "komm", "Heute backen wir Kuchen."]);
    }

    #[test]
    fn silence_is_not_voiced() {
        let mut pcm = vec![0.0f32; 16_000 * 3];
        // One second of a loud tone in the middle.
        for (i, s) in pcm.iter_mut().enumerate().skip(16_000).take(16_000) {
            *s = 0.3 * (i as f32 * 0.05).sin();
        }
        let v = voiced_frames(&pcm);
        assert!(!v[10] && !v[290], "silence voiced");
        assert!(v[150], "tone not voiced");
    }
}
