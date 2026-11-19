#!/bin/sh
# Download the vision models for tests/vision.rs (and for a deployment,
# LICHTBILD_VISION_DIR) into $1, pinned to Hugging Face revisions and
# checked by SHA-256 (the same hashes are pinned in src/vision.rs). About
# 410 MB. All Apache-2.0:
#   PaddleOCR PP-OCRv3 detection, PP-OCRv5 Latin recognition and its
#     dictionary (ONNX by monkt/paddleocr-onnx);
#   RT-DETRv2-R18, int8 (onnx-community/rtdetr_v2_r18vd-ONNX);
#   SigLIP base-patch16-256 multilingual, int8 image and text halves and
#     tokenizer (pulsejet/siglip-base-patch16-256-multilingual-onnx).
set -eu
dir=${1:?usage: fetch-vision-models.sh <dir>}
mkdir -p "$dir"
cd "$dir"
fetch() {
  [ -f "$3" ] || curl -sfL --retry 3 -A lichtbild-ci -o "$3" "https://huggingface.co/$1/resolve/$2"
}
ocr=monkt/paddleocr-onnx
ocr_rev=7b02d0a30a07ba2b92ad1ff5a8941ae2c633de65
fetch $ocr $ocr_rev/detection/v3/det.onnx ocr-det.onnx
fetch $ocr $ocr_rev/languages/latin/rec.onnx ocr-rec.onnx
fetch $ocr $ocr_rev/languages/latin/dict.txt ocr-dict.txt
det=onnx-community/rtdetr_v2_r18vd-ONNX
fetch $det 936f90b6a476c6da4dfe053fc521af55285976ba/onnx/model_quantized.onnx objects.onnx
sig=pulsejet/siglip-base-patch16-256-multilingual-onnx
sig_rev=443f7a60fb4cd33e220fcf9e6a9a1fdc948e8d82
fetch $sig $sig_rev/onnx/vision_model_quantized.onnx siglip-vision.onnx
fetch $sig $sig_rev/onnx/text_model_quantized.onnx siglip-text.onnx
fetch $sig $sig_rev/tokenizer.json siglip-tokenizer.json
cat > SHA256SUMS <<'SUMS'
ee40e80071ba3a320d4efda75f3e22047a7d049e9bf7bcaaf9daea23fc21b935  ocr-det.onnx
614ffc2d6d3902d360fad7f1b0dd455ee45e877069d14c4e51a99dc4ef144409  ocr-rec.onnx
3c0a8a79b612653c25f765271714f71281e4e955962c153e272b7b8c1d2b13ff  ocr-dict.txt
4b839c46187b77fc620c770de0be6790637b98afde9b386232b0fcf74382eb3c  objects.onnx
9dd7c669514965a47cd1013c8069f2cb144b3e45786c0cf5ec363f283780bc7c  siglip-vision.onnx
dbcd59207739cc2a222a881b766d32f37a7297e981f5f9594d36d52b741b0164  siglip-text.onnx
0d188aadabecdaec164a39e0d2307204a0ac9878093d17595dd3351fa0e42ca6  siglip-tokenizer.json
SUMS
if command -v sha256sum >/dev/null; then sha256sum -c SHA256SUMS; else shasum -a 256 -c SHA256SUMS; fi
