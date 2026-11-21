# story-bench: a small language model for story mode (spike)

Answers one question for story mode (REQ-STORY-001, DD-028): can a small
quantized instruction model write a short German story from picture facts
on the Raspberry Pi 5, and does it invent people? Kill criteria (owner,
2026-11-21): more than 90 s a story on the Pi, or names or relations not in
the pictures.

Not part of lichtbild; its own Cargo workspace. Five fixed prompts with
made-up facts (`CASES` in src/main.rs); no household data. Without `--text`
it prints timings only.

## Build

    cargo zigbuild --release --target aarch64-unknown-linux-gnu.2.36

## Models (Hugging Face, data files)

| dir | files | source |
|---|---|---|
| `qwen3-1.7b/` | `model.gguf`, `tokenizer.json` | `ggml-org/Qwen3-1.7B-GGUF` `Qwen3-1.7B-Q4_K_M.gguf`; `Qwen/Qwen3-1.7B` `tokenizer.json` (Apache-2.0) |
| `gemma3-1b/` | `model.gguf`, `tokenizer.json` | `ggml-org/gemma-3-1b-it-GGUF` `gemma-3-1b-it-Q4_K_M.gguf`; `unsloth/gemma-3-1b-it` `tokenizer.json` (Gemma terms) |

SHA-256: qwen3 model d2387ca2dbfee2ffabce7120d3770dadca0b293052bc2f0e138fdc940d9bc7b5,
tokenizer aeb13307a71acd8fe81861d94ad54ab689df773318809eed3cbe794b4492dae4;
gemma3 model 8ccc5cd1f1b3602548715ae25a66ed73fd5dc68a210412eea643eb20eb75a135,
tokenizer 4667f2089529e8e7657cfb6d1c19910ae71ff5f28aa7ab2ff2763330affad795.

Gemma 4 is not in the spike: candle 0.11.1 has no quantized Gemma 4.

## Run

    RAYON_NUM_THREADS=4 ./story-bench <model-dir> <qwen3|gemma3> [--text] [--max 300]

## Measured

Apple M4, 4 threads, 5 prompts, at most 300 tokens:

| model | prefill | generation | story (median / max) | peak RSS |
|---|---|---|---|---|
| Qwen3 1.7B Q4_K_M | 1.2-1.6 s | 46-48 tok/s | 3.8 s / 4.3 s | 2.2 GB (first case) |
| Gemma 3 1B Q4_K_M | 2.0-3.1 s | 29-30 tok/s | 7.6 s / 12.7 s | 3.0 GB (first case) |

Peak RSS grows over the run because every case reloads the model; the
first case is the figure for one story.

Text (read by hand):
- Gemma 3 1B fails: broken German, turns a named person into a cat,
  invents an innkeeper, and one story loops on one word until the token
  limit.
- Qwen3 1.7B is readable but clumsy ("Ben flügelt auf", "einen
  Videoaufnahme"), invents no names, but adds an unnamed man in one story,
  puts a sign in the wrong place, and is less funny than the templates.

Pi 5 numbers: to come (smithy).
