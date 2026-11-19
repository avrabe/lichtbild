#!/bin/sh
# Download the OCR models for tests/ocr.rs (and for a deployment) into $1,
# pinned to a Hugging Face revision and checked by SHA-256 (the same
# hashes are pinned in src/ocr.rs). PaddleOCR models, Apache-2.0, converted
# to ONNX by monkt/paddleocr-onnx: PP-OCRv3 mobile detection and PP-OCRv5
# Latin recognition with its dictionary.
set -eu
dir=${1:?usage: fetch-ocr-model.sh <dir>}
rev=7b02d0a30a07ba2b92ad1ff5a8941ae2c633de65
base=https://huggingface.co/monkt/paddleocr-onnx/resolve/$rev
mkdir -p "$dir"
cd "$dir"
fetch() {
  [ -f "$2" ] || curl -sfL --retry 3 -A lichtbild-ci -o "$2" "$base/$1"
}
fetch detection/v3/det.onnx det.onnx
fetch languages/latin/rec.onnx rec.onnx
fetch languages/latin/dict.txt dict.txt
cat > SHA256SUMS <<'SUMS'
ee40e80071ba3a320d4efda75f3e22047a7d049e9bf7bcaaf9daea23fc21b935  det.onnx
614ffc2d6d3902d360fad7f1b0dd455ee45e877069d14c4e51a99dc4ef144409  rec.onnx
3c0a8a79b612653c25f765271714f71281e4e955962c153e272b7b8c1d2b13ff  dict.txt
SUMS
if command -v sha256sum >/dev/null; then sha256sum -c SHA256SUMS; else shasum -a 256 -c SHA256SUMS; fi
