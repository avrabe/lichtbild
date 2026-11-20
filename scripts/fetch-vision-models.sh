#!/bin/sh
# Download the vision models for tests/vision.rs (and for a deployment,
# LICHTBILD_VISION_DIR) into $1, pinned to Hugging Face revisions and
# checked by SHA-256 (the same hashes are pinned in src/vision.rs). All
# Apache-2.0:
#   PaddleOCR PP-OCRv3 detection, PP-OCRv5 Latin recognition and its
#     dictionary (ONNX by monkt/paddleocr-onnx);
#   RT-DETRv2-R18 (onnx-community/rtdetr_v2_r18vd-ONNX);
#   SigLIP base-patch16-256 multilingual, image and text halves and
#     tokenizer (pulsejet/siglip-base-patch16-256-multilingual-onnx);
#   AuraFace v1, face vectors (fal/AuraFace-v1, only its glintr100.onnx:
#     the other files there are InsightFace's and not for this use).
# And YuNet 2026may, face detection (opencv/opencv_zoo, MIT), from GitHub.
# The two face models (~250 MB) are left out with LICHTBILD_FACES=0.
#
# The object and SigLIP models come as int8 (~410 MB in all) or float32
# (~1.9 GB). int8 is the default; float32 on x86-64 without VNNI, where
# rten's int8 kernels can saturate (lichtbild refuses int8 there). Set
# LICHTBILD_VISION_SET=int8 or float to choose. A file whose hash does not
# match the chosen set is fetched again, so a directory can switch sets.
set -eu
dir=${1:?usage: fetch-vision-models.sh <dir>}
set=${LICHTBILD_VISION_SET:-}
if [ -z "$set" ]; then
  set=int8
  if [ "$(uname -m)" = x86_64 ]; then
    if [ -r /proc/cpuinfo ] && grep -qE 'avx512_vnni|avx_vnni' /proc/cpuinfo; then
      set=int8
    else
      set=float
    fi
  fi
fi
case "$set" in int8|float) ;; *) echo "LICHTBILD_VISION_SET: int8 or float" >&2; exit 2 ;; esac
echo "vision models: $set set"
mkdir -p "$dir"
cd "$dir"
sum() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}
# fetch_url <url> <file> <sha256>
fetch_url() {
  if [ -f "$2" ] && [ "$(sum "$2")" = "$3" ]; then return; fi
  curl -sfL --retry 3 -A lichtbild-ci -o "$2.part" "$1"
  got=$(sum "$2.part")
  if [ "$got" != "$3" ]; then
    echo "$2: sha256 $got, expected $3" >&2
    rm -f "$2.part"
    exit 1
  fi
  mv "$2.part" "$2"
  echo "$2: OK"
}
# fetch <repo> <rev/path> <file> <sha256>, from Hugging Face
fetch() {
  fetch_url "https://huggingface.co/$1/resolve/$2" "$3" "$4"
}
o=7b02d0a30a07ba2b92ad1ff5a8941ae2c633de65
fetch monkt/paddleocr-onnx $o/detection/v3/det.onnx ocr-det.onnx ee40e80071ba3a320d4efda75f3e22047a7d049e9bf7bcaaf9daea23fc21b935
fetch monkt/paddleocr-onnx $o/languages/latin/rec.onnx ocr-rec.onnx 614ffc2d6d3902d360fad7f1b0dd455ee45e877069d14c4e51a99dc4ef144409
fetch monkt/paddleocr-onnx $o/languages/latin/dict.txt ocr-dict.txt 3c0a8a79b612653c25f765271714f71281e4e955962c153e272b7b8c1d2b13ff
det=onnx-community/rtdetr_v2_r18vd-ONNX
d=936f90b6a476c6da4dfe053fc521af55285976ba
sig=pulsejet/siglip-base-patch16-256-multilingual-onnx
s=443f7a60fb4cd33e220fcf9e6a9a1fdc948e8d82
fetch $sig $s/tokenizer.json siglip-tokenizer.json 0d188aadabecdaec164a39e0d2307204a0ac9878093d17595dd3351fa0e42ca6
if [ "$set" = int8 ]; then
  fetch $det $d/onnx/model_quantized.onnx objects.onnx 4b839c46187b77fc620c770de0be6790637b98afde9b386232b0fcf74382eb3c
  fetch $sig $s/onnx/vision_model_quantized.onnx siglip-vision.onnx 9dd7c669514965a47cd1013c8069f2cb144b3e45786c0cf5ec363f283780bc7c
  fetch $sig $s/onnx/text_model_quantized.onnx siglip-text.onnx dbcd59207739cc2a222a881b766d32f37a7297e981f5f9594d36d52b741b0164
else
  fetch $det $d/onnx/model.onnx objects.onnx 583a236ac21c95a7fd94f284fc21485e42355bfef82c27011ba78fbc09ee87e2
  fetch $sig $s/onnx/vision_model.onnx siglip-vision.onnx abe14f53c63107e6363006bbae3f6038d15d6d4a7ce233347fc0382ef09230ba
  fetch $sig $s/onnx/text_model.onnx siglip-text.onnx 8bd9cdc7038468810ae7c3bcf79f4d91927e513a5f217f1c2566862ed2558110
fi
if [ "${LICHTBILD_FACES:-1}" != 0 ]; then
  fetch fal/AuraFace-v1 af6d057c9b0ec4071d4c49c80e3539258798b609/glintr100.onnx faces-vector.onnx a7933ea5330113b01c9b60351d8f4c33003f145d8470ac5f0e52ee2effe25c60
  fetch_url https://media.githubusercontent.com/media/opencv/opencv_zoo/48b10fa505aeb51fbd127fc60a5c660040e823b7/models/face_detection_yunet/face_detection_yunet_2026may.onnx faces-detect.onnx ebafce4e3c118d6554634be5c27ab333b4c047a9a8c3faf1d7cf93101c22f0f0
fi
