//! Looking at pictures (REQ-TEXT-001, REQ-SEE-001, REQ-SEE-002, DD-023,
//! DD-024): one child process per photo (`lichtbild look`) decodes it once
//! and returns the text in it (ocr), the things in it (objects) and a
//! vector of what it shows (embed). A crash or hang in a decoder or a model
//! stays in the child. Results are stored per content hash with the
//! version of this set of models, so each photo is looked at once.
//!
//! All models sit in one directory (LICHTBILD_VISION_DIR), each file pinned
//! by SHA-256; scripts/fetch-vision-models.sh fetches them.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use image::RgbImage;
use serde::{Deserialize, Serialize};

/// The model files: PaddleOCR (monkt/paddleocr-onnx 7b02d0a3), RT-DETRv2-R18
/// int8 (onnx-community/rtdetr_v2_r18vd-ONNX 936f90b6) and SigLIP
/// base-patch16-256 multilingual int8 (pulsejet/siglip-base-patch16-256-
/// multilingual-onnx 443f7a60); all Apache-2.0.
pub const FILES: &[(&str, &str)] = &[
    (
        "ocr-det.onnx",
        "ee40e80071ba3a320d4efda75f3e22047a7d049e9bf7bcaaf9daea23fc21b935",
    ),
    (
        "ocr-rec.onnx",
        "614ffc2d6d3902d360fad7f1b0dd455ee45e877069d14c4e51a99dc4ef144409",
    ),
    (
        "ocr-dict.txt",
        "3c0a8a79b612653c25f765271714f71281e4e955962c153e272b7b8c1d2b13ff",
    ),
    (
        "objects.onnx",
        "4b839c46187b77fc620c770de0be6790637b98afde9b386232b0fcf74382eb3c",
    ),
    (
        "siglip-vision.onnx",
        "9dd7c669514965a47cd1013c8069f2cb144b3e45786c0cf5ec363f283780bc7c",
    ),
    (
        "siglip-text.onnx",
        "dbcd59207739cc2a222a881b766d32f37a7297e981f5f9594d36d52b741b0164",
    ),
    (
        "siglip-tokenizer.json",
        "0d188aadabecdaec164a39e0d2307204a0ac9878093d17595dd3351fa0e42ca6",
    ),
];

/// The version of this set of models and rules; a new one looks again.
pub const VERSION: &str = "look-2";

/// Pictures are looked at at most this large (longer side, in pixels).
pub const WORK_SIDE: u32 = 2400;
/// Wall clock per picture (the Pi 5 needs about 2–3 s).
pub const TIME_BUDGET: Duration = Duration::from_secs(120);

fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(data))
}

/// Read one model file and check it against its pinned hash.
pub fn read_pinned(dir: &Path, name: &str) -> Result<Vec<u8>> {
    let Some((_, sum)) = FILES.iter().find(|(n, _)| *n == name) else {
        bail!("{name}: not a model file");
    };
    let p = dir.join(name);
    let data = std::fs::read(&p).with_context(|| format!("vision model: {}", p.display()))?;
    let h = sha256_hex(&data);
    if h != *sum {
        bail!("{}: unknown model file (sha256 {h})", p.display());
    }
    Ok(data)
}

/// Check the model directory: every file present with its pinned hash.
pub fn check(dir: &Path) -> Result<()> {
    for (name, _) in FILES {
        read_pinned(dir, name)?;
    }
    Ok(())
}

/// What the child returns for one picture.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Look {
    pub text: crate::ocr::Text,
    pub objects: Vec<crate::objects::Object>,
    /// The picture's vector (embed::DIM signed bytes).
    pub vector: Vec<i8>,
}

/// Decode a picture: oriented, at most WORK_SIDE on its longer side.
pub fn load_picture(path: &Path, max_bytes: u64) -> Result<RgbImage> {
    let crate::media::Class::Media(format) = crate::media::classify(path) else {
        bail!("not a picture");
    };
    if format.is_video() {
        bail!("not a picture");
    }
    let mut data = Vec::new();
    File::open(path)?.take(max_bytes).read_to_end(&mut data)?;
    let img =
        crate::media::decode(&data, format, 100_000_000).map_err(|_| anyhow::anyhow!("decode"))?;
    let img = if img.width().max(img.height()) > WORK_SIDE {
        img.resize(WORK_SIDE, WORK_SIDE, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    Ok(img.to_rgb8())
}

/// Run `exe look` on one picture, killed after `timeout`.
pub fn run_child(exe: &Path, file: &Path, model_dir: &Path, timeout: Duration) -> Result<Look> {
    use std::process::{Command, Stdio};
    let mut child = Command::new(exe)
        .arg("look")
        .arg(model_dir)
        .arg(file)
        .env_clear()
        .env("RAYON_NUM_THREADS", "4")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("start look")?;
    let mut stdout = child.stdout.take().context("stdout")?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        (&mut stdout)
            .take(16 << 20)
            .read_to_end(&mut out)
            .map(|_| out)
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
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

/// The `look` child: `lichtbild look <model-dir> <file>`.
pub fn child_main(args: &[&str]) -> Result<()> {
    let [model_dir, file] = args else {
        bail!("usage: look <model-dir> <file>");
    };
    limit_self(TIME_BUDGET.as_secs() * 4);
    let dir = Path::new(model_dir);
    let img = load_picture(Path::new(file), 256 << 20)?;
    let text = crate::ocr::Engine::load(dir)?.read(&img)?;
    let objects =
        crate::objects::Detector::new(rten::Model::load(read_pinned(dir, "objects.onnx")?)?)
            .detect(&img)?;
    let vector = crate::embed::ImageEncoder::new(rten::Model::load(read_pinned(
        dir,
        "siglip-vision.onnx",
    )?)?)
    .encode(&img)?;
    serde_json::to_writer(
        std::io::stdout().lock(),
        &Look {
            text,
            objects,
            vector,
        },
    )?;
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
    // OCR measured 0.9 GB on the Pi 5 at 2400 px; the int8 models add
    // about 0.3 GB; room for a 100 MP decode.
    #[cfg(target_os = "linux")]
    set(libc::RLIMIT_AS, 4 << 30);
}

/// What one pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct PassReport {
    pub looked: u64,
    pub with_text: u64,
    pub with_things: u64,
    pub failed: u64,
    /// Left for the next pass because the deadline came.
    pub remaining: u64,
}

impl PassReport {
    pub fn to_text(&self) -> String {
        format!(
            "looked at {} photos: {} with text, {} with things, {} failed, {} left for the next pass",
            self.looked, self.with_text, self.with_things, self.failed, self.remaining
        )
    }
}

fn short_reason(e: &anyhow::Error) -> String {
    let s = e.to_string();
    let s: String = s
        .split(':')
        .next()
        .unwrap_or("error")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == ' ' || *c == '-')
        .take(40)
        .collect();
    s.trim().to_string()
}

/// Look at the photos not yet looked at with VERSION, newest first, one
/// child at a time, until `deadline`. Every result, failures included, is
/// stored under the photo's content hash, so the pass resumes.
pub fn run_pass(
    cfg: &crate::config::Config,
    idx: &crate::index::Index,
    exe: &Path,
    deadline: Option<Instant>,
    progress: &dyn Fn(u64, u64, &PassReport),
) -> Result<PassReport> {
    let mut rep = PassReport::default();
    let Some(dir) = &cfg.vision_dir else {
        return Ok(rep);
    };
    let todo = idx.photos_to_look(VERSION)?;
    let total = todo.len() as u64;
    if total > 0 {
        tracing::info!("looking at {total} photos");
    }
    let started = Instant::now();
    let mut said = Instant::now();
    for (n, (_, rec)) in todo.into_iter().enumerate() {
        if n > 0 && said.elapsed() >= Duration::from_secs(600) {
            said = Instant::now();
            tracing::info!(
                "looked at {n} of {total} photos ({} with text, {} with things, {} failed) in {} min",
                rep.with_text,
                rep.with_things,
                rep.failed,
                started.elapsed().as_secs() / 60
            );
        }
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
        match run_child(exe, &path, dir, TIME_BUDGET) {
            Ok(look) => {
                rep.looked += 1;
                rep.with_text += u64::from(look.text.state == "ok");
                rep.with_things += u64::from(!look.objects.is_empty());
                idx.set_look(&rec.hash, VERSION, &look)?;
            }
            Err(e) => {
                rep.failed += 1;
                idx.set_look_failed(&rec.hash, VERSION, &short_reason(&e))?;
            }
        }
        progress(n as u64 + 1, total, &rep);
    }
    Ok(rep)
}
