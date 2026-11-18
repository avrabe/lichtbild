#!/bin/sh
# Download Whisper tiny q8_0 for tests/transcribe.rs into $1, pinned to a
# Hugging Face revision and checked by SHA-256 (the model file's hash is
# also pinned in src/stt/mod.rs). Model weights: MIT (OpenAI Whisper);
# converted to GGUF by lmz/candle-whisper.
set -eu
dir=${1:?usage: fetch-test-model.sh <dir>}
rev=02d8350e5402f18725eadb6101b4963d181b0b5e
base=https://huggingface.co/lmz/candle-whisper/resolve/$rev
mkdir -p "$dir"
cd "$dir"
fetch() {
  [ -f "$2" ] || curl -sfL --retry 3 -A lichtbild-ci -o "$2" "$base/$1"
}
fetch config-tiny.json config.json
fetch tokenizer-tiny.json tokenizer.json
fetch model-tiny-q80.gguf model.gguf
cat > SHA256SUMS <<'SUMS'
ffdccec4f3211f4c63310f2b7098f309fe70f3952cedc5e4d11e43f5b2379b98  config.json
dfc530298b6fbed1a97c6472c575b026453706e2a204c7f7038f2c9d208b0759  tokenizer.json
edcc907db61aef092f1244dc1e53c55056b472be343e9bbb4dc12ebd4740392f  model.gguf
SUMS
if command -v sha256sum >/dev/null; then sha256sum -c SHA256SUMS; else shasum -a 256 -c SHA256SUMS; fi
