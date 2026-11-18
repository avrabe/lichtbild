//! stt-bench: time pure-Rust Whisper (candle) on one media file.
//!
//! Usage: stt-bench <model-dir> <media-file> [--language de] [--text]
//!
//! <model-dir> holds config.json, tokenizer.json and either
//! model.safetensors (f32) or model.gguf (quantized q8_0). The audio track
//! is decoded with symphonia (MP4/MOV AAC, WAV), mixed to channel 0 and
//! resampled to 16 kHz with rubato. Without --text only timings, segment
//! counts and the detected language are printed, never what was said.
//! Threads: RAYON_NUM_THREADS (candle's CPU kernels).

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod audio;
mod multilingual;
mod whisper;

pub use candle_transformers::models::whisper as m;

use std::time::Instant;

use anyhow::{bail, Context, Result};
use candle::{Device, Tensor};
use candle_nn::VarBuilder;
use tokenizers::Tokenizer;

use whisper::{token_id, Decoder, Model};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut pos = Vec::new();
    let mut language = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--language" => language = it.next().cloned(),
            "--text" => whisper::TEXT.store(true, std::sync::atomic::Ordering::Relaxed),
            _ => pos.push(a.clone()),
        }
    }
    let [dir, input] = pos.as_slice() else {
        bail!("usage: stt-bench <model-dir> <media-file> [--language de] [--text]");
    };
    let dir = std::path::Path::new(dir);
    println!("build: {} {}", if cfg!(feature = "mimalloc") { "mimalloc" } else { "system allocator" }, if cfg!(target_env = "musl") { "musl" } else { "glibc/other" });
    let device = Device::Cpu;
    let total = Instant::now();

    let t = Instant::now();
    let (pcm, sr) = audio::pcm_decode(input).context("decode audio")?;
    let pcm = if sr == m::SAMPLE_RATE as u32 {
        pcm
    } else {
        audio::resample(&pcm, sr, m::SAMPLE_RATE as u32).context("resample")?
    };
    let audio_s = pcm.len() as f64 / m::SAMPLE_RATE as f64;
    println!("audio: {audio_s:.1} s (source {sr} Hz), decoded in {:.2} s", t.elapsed().as_secs_f64());

    let config: m::Config =
        serde_json::from_str(&std::fs::read_to_string(dir.join("config.json"))?)?;
    let tokenizer = Tokenizer::from_file(dir.join("tokenizer.json")).map_err(anyhow::Error::msg)?;
    let mel_bytes = match config.num_mel_bins {
        80 => include_bytes!("melfilters.bytes").as_slice(),
        128 => include_bytes!("melfilters128.bytes").as_slice(),
        n => bail!("unexpected num_mel_bins {n}"),
    };
    let mut filters = vec![0f32; mel_bytes.len() / 4];
    <byteorder::LittleEndian as byteorder::ByteOrder>::read_f32_into(mel_bytes, &mut filters);

    let t = Instant::now();
    let mel = m::audio::pcm_to_mel(&config, &pcm, &filters);
    let n = mel.len() / config.num_mel_bins;
    let mel = Tensor::from_vec(mel, (1, config.num_mel_bins, n), &device)?;
    println!("mel: {:?} in {:.2} s", mel.dims(), t.elapsed().as_secs_f64());

    let t = Instant::now();
    let gguf = dir.join("model.gguf");
    let (mut model, kind) = if gguf.is_file() {
        let vb = candle_transformers::quantized_var_builder::VarBuilder::from_gguf(&gguf, &device)?;
        (Model::Quantized(m::quantized_model::Whisper::load(&vb, config)?), "gguf q8_0")
    } else {
        // SAFETY: the weights file is not modified while mapped.
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[dir.join("model.safetensors")], m::DTYPE, &device)?
        };
        (Model::Normal(m::model::Whisper::load(&vb, config)?), "safetensors f32")
    };
    println!("model: {kind}, loaded in {:.2} s", t.elapsed().as_secs_f64());

    let t = Instant::now();
    let lang = match language {
        Some(l) => token_id(&tokenizer, &format!("<|{l}|>"))?,
        None => multilingual::detect_language(&mut model, &tokenizer, &mel)?,
    };
    println!("language token {lang}, in {:.2} s", t.elapsed().as_secs_f64());

    let t = Instant::now();
    let mut dc = Decoder::new(model, tokenizer, 299792458, &device, Some(lang), None, true, None, false)?;
    let segments = dc.run(&mel)?;
    let secs = t.elapsed().as_secs_f64();
    let words: usize = segments.iter().map(|s| s.dr.text.split_whitespace().count()).sum();
    println!(
        "transcribed: {} segments, {words} words, in {secs:.1} s; RTF {:.2} (transcription) / {:.2} (total)",
        segments.len(),
        secs / audio_s,
        total.elapsed().as_secs_f64() / audio_s
    );
    Ok(())
}
