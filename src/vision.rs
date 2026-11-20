//! Looking at pictures (REQ-TEXT-001, REQ-SEE-001, REQ-SEE-002, DD-023,
//! DD-024): a child process (`lichtbild look`) loads the models once for a
//! batch of photos, decodes each once and returns the text in it (ocr), the
//! things in it (objects) and a vector of what it shows (embed). A crash or
//! hang in a decoder or a model stays in the child and fails only the photo
//! it was on; a new child takes the rest. Results are stored per content hash with the
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

/// Which set of the object and SigLIP models a directory holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Precision {
    /// int8 (dynamic quantization): half the time, a quarter of the size.
    Int8,
    /// float32: for x86-64 without VNNI, where rten's int8 kernels can
    /// saturate on models not quantized with reduced range (DD-024).
    Float,
}

/// The model files and the hashes they are accepted with: PaddleOCR
/// (monkt/paddleocr-onnx 7b02d0a3), RT-DETRv2-R18
/// (onnx-community/rtdetr_v2_r18vd-ONNX 936f90b6) and SigLIP
/// base-patch16-256 multilingual (pulsejet/siglip-base-patch16-256-
/// multilingual-onnx 443f7a60), each in int8 or float32; all Apache-2.0.
pub const FILES: &[(&str, &str, Option<Precision>)] = &[
    (
        "ocr-det.onnx",
        "ee40e80071ba3a320d4efda75f3e22047a7d049e9bf7bcaaf9daea23fc21b935",
        None,
    ),
    (
        "ocr-rec.onnx",
        "614ffc2d6d3902d360fad7f1b0dd455ee45e877069d14c4e51a99dc4ef144409",
        None,
    ),
    (
        "ocr-dict.txt",
        "3c0a8a79b612653c25f765271714f71281e4e955962c153e272b7b8c1d2b13ff",
        None,
    ),
    (
        "objects.onnx",
        "4b839c46187b77fc620c770de0be6790637b98afde9b386232b0fcf74382eb3c",
        Some(Precision::Int8),
    ),
    (
        "objects.onnx",
        "583a236ac21c95a7fd94f284fc21485e42355bfef82c27011ba78fbc09ee87e2",
        Some(Precision::Float),
    ),
    (
        "siglip-vision.onnx",
        "9dd7c669514965a47cd1013c8069f2cb144b3e45786c0cf5ec363f283780bc7c",
        Some(Precision::Int8),
    ),
    (
        "siglip-vision.onnx",
        "abe14f53c63107e6363006bbae3f6038d15d6d4a7ce233347fc0382ef09230ba",
        Some(Precision::Float),
    ),
    (
        "siglip-text.onnx",
        "dbcd59207739cc2a222a881b766d32f37a7297e981f5f9594d36d52b741b0164",
        Some(Precision::Int8),
    ),
    (
        "siglip-text.onnx",
        "8bd9cdc7038468810ae7c3bcf79f4d91927e513a5f217f1c2566862ed2558110",
        Some(Precision::Float),
    ),
    (
        "siglip-tokenizer.json",
        "0d188aadabecdaec164a39e0d2307204a0ac9878093d17595dd3351fa0e42ca6",
        None,
    ),
];

/// The version of this set of models and rules, without the precision
/// (see `version`); a new one looks again.
pub const VERSION: &str = "look-3";

/// The stored version for a model set: a picture looked at with int8
/// models is looked at again with float32 ones, so vectors never mix.
pub fn version(p: Precision) -> String {
    match p {
        Precision::Int8 => format!("{VERSION}-int8"),
        Precision::Float => format!("{VERSION}-f32"),
    }
}

/// Whether rten's int8 matrix kernels are exact on this CPU: on x86-64
/// without VNNI they may saturate.
pub fn int8_safe() -> bool {
    !rten_gemm::GemmExecutor::<u8, i8, i32>::default().may_saturate()
}

/// Pictures are looked at at most this large (longer side, in pixels).
pub const WORK_SIDE: u32 = 2400;
/// Wall clock per picture (the Pi 5 needs about 2–3 s).
pub const TIME_BUDGET: Duration = Duration::from_secs(120);

fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(data))
}

/// Read one model file and check it against its pinned hashes; returns
/// the bytes and the precision the hash stands for.
fn read_with(dir: &Path, name: &str) -> Result<(Vec<u8>, Option<Precision>)> {
    if !FILES.iter().any(|(n, _, _)| *n == name) {
        bail!("{name}: not a model file");
    }
    let p = dir.join(name);
    let data = std::fs::read(&p).with_context(|| format!("vision model: {}", p.display()))?;
    let h = sha256_hex(&data);
    match FILES.iter().find(|(n, sum, _)| *n == name && *sum == h) {
        Some((_, _, precision)) => Ok((data, *precision)),
        None => bail!("{}: unknown model file (sha256 {h})", p.display()),
    }
}

/// Read one model file and check it against its pinned hashes.
pub fn read_pinned(dir: &Path, name: &str) -> Result<Vec<u8>> {
    Ok(read_with(dir, name)?.0)
}

/// Check the model directory: every file present with a pinned hash, the
/// object and SigLIP models of one precision, and int8 only where its
/// kernels are exact. Returns the precision.
pub fn check(dir: &Path) -> Result<Precision> {
    let mut names: Vec<&str> = FILES.iter().map(|(n, _, _)| *n).collect();
    names.dedup();
    let mut set = None;
    for name in names {
        if let (_, Some(p)) = read_with(dir, name)? {
            match set {
                None => set = Some(p),
                Some(q) if q != p => bail!("{}: int8 and float32 model files mixed", dir.display()),
                Some(_) => {}
            }
        }
    }
    let set = set.context("no object or SigLIP models")?;
    if set == Precision::Int8 && !int8_safe() {
        bail!(
            "{}: the int8 vision models give wrong answers on this CPU (x86-64 without VNNI); \
             use the float32 set (scripts/fetch-vision-models.sh picks it on such a machine)",
            dir.display()
        );
    }
    Ok(set)
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
    run_child_as(exe, "look", file, model_dir, timeout)
}

/// Run `exe <mode> <model_dir> <file>` (`look` or `faces`) on one picture,
/// killed after `timeout`, and read its answer.
pub fn run_child_as<T: serde::de::DeserializeOwned + Send + 'static>(
    exe: &Path,
    mode: &str,
    file: &Path,
    model_dir: &Path,
    timeout: Duration,
) -> Result<T> {
    let mut out = None;
    run_batch(
        exe,
        mode,
        model_dir,
        &[file.to_path_buf()],
        timeout,
        &mut |_, r| {
            out = Some(r);
            Ok(())
        },
    )?;
    out.context("no answer")?
}

/// Photos per child: the models are read, checked and loaded once per
/// batch (on an M4 that took as long as looking at a photo).
pub const BATCH: usize = 32;

/// One answer of a child, a line of JSON per picture.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Answer<T> {
    Ok(T),
    Err(String),
}

/// At most this much JSON per picture.
const MAX_ANSWER: u64 = 16 << 20;

/// Run `exe <mode> <model_dir> <files…>` and hand each picture's answer to
/// `each` with its position, in order. A picture gets `timeout`; a child
/// that hangs is killed, one that dies is gone, and either way that
/// picture fails and a new child takes the rest.
pub fn run_batch<T: serde::de::DeserializeOwned + Send + 'static>(
    exe: &Path,
    mode: &str,
    model_dir: &Path,
    files: &[std::path::PathBuf],
    timeout: Duration,
    each: &mut dyn FnMut(usize, Result<T>) -> Result<()>,
) -> Result<()> {
    use std::io::BufRead;
    use std::process::{Command, Stdio};
    let mut at = 0;
    while at < files.len() {
        // A child reads its arguments as UTF-8: a path that is not fails
        // here, alone, and never reaches a child (where it would fail every
        // photo before it in the batch).
        if files[at].to_str().is_none() {
            each(at, Err(anyhow::anyhow!("path not UTF-8")))?;
            at += 1;
            continue;
        }
        let end = files[at..]
            .iter()
            .position(|f| f.to_str().is_none())
            .map_or(files.len(), |p| at + p);
        let mut child = Command::new(exe)
            .arg(mode)
            .arg(model_dir)
            .args(&files[at..end])
            .env_clear()
            .env("RAYON_NUM_THREADS", "4")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("start {mode}"))?;
        let stdout = child.stdout.take().context("stdout")?;
        let (tx, rx) = std::sync::mpsc::channel::<Result<Answer<T>>>();
        std::thread::spawn(move || {
            let mut r = std::io::BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                let n = match (&mut r).take(MAX_ANSWER).read_until(b'\n', &mut line) {
                    Ok(n) => n,
                    Err(e) => {
                        let _ = tx.send(Err(e.into()));
                        return;
                    }
                };
                if n == 0 {
                    return;
                }
                let a = if line.last() != Some(&b'\n') {
                    Err(anyhow::anyhow!("answer too long"))
                } else {
                    serde_json::from_slice(&line).context("child output")
                };
                let bad = a.is_err();
                if tx.send(a).is_err() || bad {
                    return;
                }
            }
        });
        let mut broke = false;
        while at < end {
            match rx.recv_timeout(timeout) {
                Ok(Ok(Answer::Ok(v))) => each(at, Ok(v))?,
                Ok(Ok(Answer::Err(e))) => each(at, Err(anyhow::anyhow!(e)))?,
                Ok(Err(e)) => {
                    each(at, Err(e))?;
                    broke = true;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    each(at, Err(anyhow::anyhow!("timeout")))?;
                    broke = true;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = child.kill();
                    let code = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
                    each(at, Err(anyhow::anyhow!("exit {code}")))?;
                    at += 1;
                    break;
                }
            }
            at += 1;
            if broke {
                break;
            }
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    Ok(())
}

/// The child's side of a batch: one answer line per file, in order.
pub(crate) fn answer_each<T: Serialize>(
    files: &[&str],
    mut one: impl FnMut(&Path) -> Result<T>,
) -> Result<()> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    for f in files {
        let a = match one(Path::new(f)) {
            Ok(v) => Answer::Ok(v),
            Err(e) => Answer::Err(short_reason(&e)),
        };
        serde_json::to_writer(&mut out, &a)?;
        out.write_all(b"\n")?;
        out.flush()?;
    }
    Ok(())
}

/// What `in_batches` hands each photo's answer to: its position, its
/// record, the answer.
pub type Each<'a, T> = dyn FnMut(usize, &crate::index::Record, Result<T>) -> Result<()> + 'a;

/// The photos of `todo` that still resolve inside their root (SSC-004),
/// in batches, until `deadline`: `each` gets every photo's answer with its
/// position in `todo`. Returns how many were left for the next pass.
#[allow(clippy::too_many_arguments)]
pub fn in_batches<T: serde::de::DeserializeOwned + Send + 'static>(
    cfg: &crate::config::Config,
    todo: &[(u64, crate::index::Record)],
    exe: &Path,
    mode: &str,
    model_dir: &Path,
    timeout: Duration,
    deadline: Option<Instant>,
    each: &mut Each<'_, T>,
) -> Result<u64> {
    for (c, chunk) in todo.chunks(BATCH).enumerate() {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Ok((todo.len() - c * BATCH) as u64);
        }
        let mut files = Vec::new();
        let mut at = Vec::new();
        for (i, (_, rec)) in chunk.iter().enumerate() {
            let Some(root) = cfg.roots.iter().find(|r| r.label == rec.label) else {
                continue;
            };
            let rel = crate::index::rel_from_bytes(&rec.rel);
            if let Ok(path) = crate::source::resolve(&root.path, &rel) {
                files.push(path);
                at.push(c * BATCH + i);
            }
        }
        run_batch(exe, mode, model_dir, &files, timeout, &mut |k, r| {
            let n = at[k];
            each(n, &todo[n].1, r)
        })?;
    }
    Ok(0)
}

/// The `look` child: `lichtbild look <model-dir> <file>…`, one answer
/// line per file; the models are loaded once.
pub fn child_main(args: &[&str]) -> Result<()> {
    let [model_dir, files @ ..] = args else {
        bail!("usage: look <model-dir> <file>…");
    };
    if files.is_empty() {
        bail!("usage: look <model-dir> <file>…");
    }
    limit_self(TIME_BUDGET.as_secs() * 4 * files.len() as u64);
    let dir = Path::new(model_dir);
    let ocr = crate::ocr::Engine::load(dir)?;
    let det = crate::objects::Detector::new(rten::Model::load(read_pinned(dir, "objects.onnx")?)?);
    let enc = crate::embed::ImageEncoder::new(rten::Model::load(read_pinned(
        dir,
        "siglip-vision.onnx",
    )?)?);
    answer_each(files, |f| {
        let img = load_picture(f, 256 << 20)?;
        Ok(Look {
            text: ocr.read(&img)?,
            objects: det.detect(&img)?,
            vector: enc.encode(&img)?,
        })
    })
}

pub(crate) fn limit_self(cpu_seconds: u64) {
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

pub(crate) fn short_reason(e: &anyhow::Error) -> String {
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

/// Look at the photos not yet looked at with this version, newest first, one
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
    let version = cfg.vision_version.as_str();
    let todo = idx.photos_to_look(version)?;
    let total = todo.len() as u64;
    if total > 0 {
        tracing::info!("looking at {total} photos");
    }
    let started = Instant::now();
    let mut said = Instant::now();
    rep.remaining = in_batches::<Look>(
        cfg,
        &todo,
        exe,
        "look",
        dir,
        TIME_BUDGET,
        deadline,
        &mut |n, rec, r| {
            match r {
                Ok(look) => {
                    rep.looked += 1;
                    rep.with_text += u64::from(look.text.state == "ok");
                    rep.with_things += u64::from(!look.objects.is_empty());
                    idx.set_look(&rec.hash, version, &look)?;
                }
                Err(e) => {
                    rep.failed += 1;
                    idx.set_look_failed(&rec.hash, version, &short_reason(&e))?;
                }
            }
            progress(n as u64 + 1, total, &rep);
            if said.elapsed() >= Duration::from_secs(600) {
                said = Instant::now();
                tracing::info!(
                    "looked at {} of {total} photos ({} with text, {} with things, {} failed) in {} min",
                    n + 1,
                    rep.with_text,
                    rep.with_things,
                    rep.failed,
                    started.elapsed().as_secs() / 60
                );
            }
            Ok(())
        },
    )?;
    Ok(rep)
}
