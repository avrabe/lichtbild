//! story-bench: how fast does a small quantized instruction model write a
//! short German story from picture facts, and what does it invent?
//!
//! Usage: story-bench <model-dir> <qwen3|gemma3> [--text] [--max N]
//! <model-dir> holds model.gguf and tokenizer.json. Five fixed prompts with
//! made-up facts (no household data). Prints per prompt: prompt tokens,
//! prefill time, generated tokens, generation speed and total time; with
//! --text the story too, and the capitalised words not among the facts
//! (candidates for invented names).

use std::io::Write;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use candle::quantized::gguf_file;
use candle::{Device, Tensor};
use candle_transformers::generation::{LogitsProcessor, Sampling};

/// (names allowed, facts per picture) for each prompt. All made up.
const CASES: [(&str, &[&str]); 5] = [
    (
        "Anna, Ben",
        &[
            "Samstag im Juli, Seestadt; Anna und Ben; ein Kuchen",
            "Seestadt; eine Katze",
            "Sonntag, Bergdorf; ein Schild: „Eiscafé Venezia“",
            "Bergdorf; Anna; im Video gesagt: „Schau mal, ein Eichhörnchen“",
        ],
    ),
    (
        "Oma Hilde",
        &[
            "Dienstag im Dezember, Waldheim; Oma Hilde; ein Weihnachtsbaum",
            "Waldheim; ein Hund; ein Schlitten",
        ],
    ),
    (
        "Cleo, Max, Paul",
        &[
            "Freitag im Mai, Hafenstadt; Cleo und Max; ein Boot",
            "Hafenstadt; Möwen",
            "Hafenstadt; Paul; ein Schild: „Fischbrötchen 3 €“",
            "Samstag, Dünenhof; ein Fahrrad",
            "Dünenhof; Cleo, Max und Paul; ein Drachen",
        ],
    ),
    (
        "",
        &[
            "Mittwoch im März, Altstadt; ein Regenschirm",
            "Altstadt; eine Uhr; ein Schild: „Rathaus“",
        ],
    ),
    (
        "Lena",
        &[
            "Sonntag im August, Bergsee; Lena; ein Rucksack",
            "Bergsee; Lena; eine Kuh",
            "Bergsee; im Video gesagt: „Das Wasser ist eiskalt!“",
        ],
    ),
];

fn prompt(names: &str, facts: &[&str]) -> String {
    let mut p = String::from(
        "Schreibe eine kurze, lustige Märchengeschichte auf Deutsch (höchstens 120 Wörter) zu diesen Fotos, Bild für Bild, in der Reihenfolge. ",
    );
    if names.is_empty() {
        p += "Auf den Fotos sind keine bekannten Personen; erfinde keine Namen. ";
    } else {
        p += &format!(
            "Die einzigen Personen sind: {names}. Erfinde keine weiteren Personen, Namen oder Verwandtschaften. "
        );
    }
    p += "Fakten:\n";
    for (i, f) in facts.iter().enumerate() {
        p += &format!("Bild {}: {f}\n", i + 1);
    }
    p
}

fn peak_rss_mb() -> f64 {
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
    let v = u.ru_maxrss as f64;
    if cfg!(target_os = "macos") {
        v / 1_048_576.0
    } else {
        v / 1024.0
    }
}

enum Model {
    Qwen3(candle_transformers::models::quantized_qwen3::ModelWeights),
    Gemma3(candle_transformers::models::quantized_gemma3::ModelWeights),
}

impl Model {
    fn load(path: &std::path::Path, arch: &str, dev: &Device) -> Result<Model> {
        let mut f = std::fs::File::open(path).with_context(|| path.display().to_string())?;
        let ct = gguf_file::Content::read(&mut f)?;
        Ok(match arch {
            "qwen3" => Model::Qwen3(
                candle_transformers::models::quantized_qwen3::ModelWeights::from_gguf(ct, &mut f, dev)?,
            ),
            "gemma3" => Model::Gemma3(
                candle_transformers::models::quantized_gemma3::ModelWeights::from_gguf(ct, &mut f, dev)?,
            ),
            _ => bail!("arch: qwen3 or gemma3"),
        })
    }

    fn forward(&mut self, x: &Tensor, pos: usize) -> Result<Tensor> {
        Ok(match self {
            Model::Qwen3(m) => m.forward(x, pos)?,
            Model::Gemma3(m) => m.forward(x, pos)?,
        })
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        bail!("usage: story-bench <model-dir> <qwen3|gemma3> [--text] [--max N]");
    }
    let dir = std::path::PathBuf::from(&args[1]);
    let arch = args[2].as_str();
    let text = args.iter().any(|a| a == "--text");
    let max: usize = args
        .iter()
        .position(|a| a == "--max")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    let dev = Device::Cpu;
    let tok = tokenizers::Tokenizer::from_file(dir.join("tokenizer.json")).map_err(anyhow::Error::msg)?;
    let (stop, wrap): (&str, fn(&str) -> String) = match arch {
        "qwen3" => ("<|im_end|>", |p| {
            format!("<|im_start|>user\n{p} /no_think<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n")
        }),
        "gemma3" => ("<end_of_turn>", |p| {
            format!("<bos><start_of_turn>user\n{p}<end_of_turn>\n<start_of_turn>model\n")
        }),
        _ => bail!("arch: qwen3 or gemma3"),
    };
    let stop_id = tok.token_to_id(stop).context("stop token")?;
    println!(
        "story-bench {arch} threads={} max={max}",
        std::env::var("RAYON_NUM_THREADS").unwrap_or_else(|_| "default".into())
    );
    let mut totals = Vec::new();
    for (i, (names, facts)) in CASES.iter().enumerate() {
        let t0 = Instant::now();
        let mut model = Model::load(&dir.join("model.gguf"), arch, &dev)?;
        let load = t0.elapsed();
        let p = wrap(&prompt(names, facts));
        let ids = tok.encode(p.as_str(), false).map_err(anyhow::Error::msg)?.get_ids().to_vec();
        let mut lp = LogitsProcessor::from_sampling(
            42 + i as u64,
            Sampling::TopP { p: 0.9, temperature: 0.8 },
        );
        let t1 = Instant::now();
        let input = Tensor::new(ids.as_slice(), &dev)?.unsqueeze(0)?;
        let logits = model.forward(&input, 0)?.squeeze(0)?;
        let mut next = lp.sample(&logits)?;
        let prefill = t1.elapsed();
        let mut out = vec![next];
        let t2 = Instant::now();
        while out.len() < max && next != stop_id {
            let input = Tensor::new(&[next], &dev)?.unsqueeze(0)?;
            let logits = model.forward(&input, ids.len() + out.len() - 1)?.squeeze(0)?;
            next = lp.sample(&logits)?;
            out.push(next);
        }
        let gen = t2.elapsed();
        let story = tok.decode(&out, true).map_err(anyhow::Error::msg)?;
        let total = prefill + gen;
        totals.push(total.as_secs_f64());
        println!(
            "case {}: load {:.1} s, prompt {} tok, prefill {:.1} s, gen {} tok in {:.1} s ({:.1} tok/s), story {:.1} s, words {}, peak RSS {:.0} MB",
            i + 1,
            load.as_secs_f64(),
            ids.len(),
            prefill.as_secs_f64(),
            out.len(),
            gen.as_secs_f64(),
            out.len() as f64 / gen.as_secs_f64(),
            total.as_secs_f64(),
            story.split_whitespace().count(),
            peak_rss_mb()
        );
        if text {
            let allowed: String = format!("{names} {}", facts.join(" "));
            let unknown: Vec<&str> = story
                .split(|c: char| !c.is_alphanumeric() && c != '-')
                .filter(|w| w.chars().next().is_some_and(|c| c.is_uppercase()))
                .filter(|w| !allowed.contains(w))
                .collect();
            println!("--- story {}\n{}\n--- not in the facts: {:?}\n", i + 1, story.trim(), unknown);
        }
        std::io::stdout().flush()?;
    }
    totals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "stories: median {:.1} s, max {:.1} s (kill criterion: more than 90 s)",
        totals[totals.len() / 2],
        totals[totals.len() - 1]
    );
    Ok(())
}
