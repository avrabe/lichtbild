# stt-bench: pure-Rust Whisper on the Pi (spike)

Answers one question before v0.4.0 (transcripts): how fast does candle's
Whisper transcribe household video on the Raspberry Pi? The plan's
threshold: real-time factor (RTF, compute time / audio length) at most 2
for `base`, else whisper.cpp in a child process.

Not part of lichtbild; its own Cargo workspace. The decoder is candle's
Whisper example (MIT OR Apache-2.0, see the file headers) with the hub
download and clap removed and a timing harness added.

## Build

    cargo zigbuild --release --target aarch64-unknown-linux-musl

## Models (Hugging Face, data files)

| dir | files | source |
|---|---|---|
| `tiny-q80/` | `config.json`, `tokenizer.json`, `model.gguf` | `lmz/candle-whisper` main: `config-tiny.json`, `tokenizer-tiny.json`, `model-tiny-q80.gguf` |
| `base/` | `config.json`, `tokenizer.json`, `model.safetensors` | `openai/whisper-base` revision `refs/pr/22` |

SHA-256: `model-tiny-q80.gguf` edcc907db61aef092f1244dc1e53c55056b472be343e9bbb4dc12ebd4740392f,
base `model.safetensors` 07cadb9f25677c8d50df603e66a98fbd842cce45047139baeb16e6219a1e807b.

## Run

    ./fetch-media.sh        # open test media with reference texts
    RAYON_NUM_THREADS=4 ./stt-bench <model-dir> <video.mp4|.mov|.mp3> [--language de] [--text] > out.txt
    python3 wer.py galerie.txt out.txt     # needs --text; free start and end

Prints audio length, decode, mel, model-load and language-detection times,
then segments, words and the RTF. Without `--text` it never prints what was
said; with `--text` the transcript goes to stdout only.

## Measured

Apple M4, 4 threads, 67 s of synthetic German speech (macOS `say -v Anna`,
AAC 48 kHz in MP4):

| model | RTF | result |
|---|---|---|
| tiny q8_0 | 0.15 | first window right, second window a repetition loop |
| base f32 | 0.22 | the text word for word, two small slips ("Enden" for "Enten") |

Apple M4, 4 threads, open media (`fetch-media.sh`); WER from `wer.py`
(lower case, ß as ss, punctuation dropped, words before and after the
reference free):

| clip | audio | model | RTF | WER | notes |
|---|---|---|---|---|---|
| Kafka, German | 182 s | base | 0.40 | 98.6% | detected English from the English LibriVox intro, then wrote English |
| Kafka, `--language de` | 182 s | base | 0.24 | 42.4% | readable; words cut at the 30 s seams; a loop after the end |
| Kafka, `--language de` | 182 s | tiny q8_0 | 0.04 | 76.6% | |
| Sterntaler, German | 146 s | base | 0.29 | (no reference) | detected German |
| Tears of Steel, English | 734 s | base | 0.10 | 56.7% | hallucinates over music ("Sssss", "I'm I'm") |
| Tears of Steel, English | 734 s | tiny q8_0 | 0.03 | 78.2% | |

What v0.4.0 has to do beyond the example, whatever the engine:

- Detect the language per 30 s window, not once from the first window.
- Skip silence and music before decoding (VAD); that is where the loops
  and the made-up text come from.
- Seek to the last complete timestamp instead of a fixed 30 s step, so
  words at the window seams survive.
