# ocr-bench (spike for v0.7: text in pictures)

Which OCR engine reads German text in photos, and how fast on a Pi 5.
The test pictures are drawn at run time from German sentences written for
this spike (`LINES` in `src/main.rs`) with a font you name. The right text
is known exactly, and no image is stored or committed.

## Models (not committed)

```sh
M=ocr-models; mkdir -p $M/paddle $M/ocrs
P=https://huggingface.co/monkt/paddleocr-onnx/resolve/7b02d0a30a07ba2b92ad1ff5a8941ae2c633de65
curl -fL -o $M/paddle/detection_v3_det.onnx      $P/detection/v3/det.onnx
curl -fL -o $M/paddle/detection_v5_det.onnx      $P/detection/v5/det.onnx
curl -fL -o $M/paddle/languages_latin_rec.onnx   $P/languages/latin/rec.onnx
curl -fL -o $M/paddle/languages_latin_dict.txt   $P/languages/latin/dict.txt
O=https://huggingface.co/robertknight/ocrs/resolve/df0edd170279ab971b53e094c627255a87e1a503
curl -fL -o $M/ocrs/text-detection.rten   $O/text-detection-ssfbcj81.rten
curl -fL -o $M/ocrs/text-recognition.rten $O/text-rec-checkpoint-s52qdbqt.rten
shasum -a 256 -c SHA256SUMS.models
```

PaddleOCR models: Apache-2.0. ocrs models: CC-BY-SA-4.0.

## Run

```sh
RAYON_NUM_THREADS=4 ./ocr-bench ocr-models /path/a.ttf,/path/b.ttf [ocrs|paddle-v3|paddle-v5 ...]
```

On Debian, `/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf,/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf`.
The output gives model load time, seconds per picture, the character error
rate (CER) and the share of ä ö ü Ä Ö Ü ß read right. `OCR_BENCH_TEXT=1`
prints what was read. That is safe here, because all text is synthetic.

## Results on an M4 with 4 threads (2026-11-19)

The set is 4 fonts (Arial, Georgia, Verdana, Times New Roman) × 6 pictures:
- pages at 36 and 24 px;
- a page tilted 3°;
- a small sign in a 2000×1500 scene;
- a tightly spaced page, flat and tilted 2°.

That makes 24 pictures and 168 lines.

| engine | per picture (median) | CER | umlauts/ß right |
|---|---|---|---|
| ocrs 0.14 | 1.45 s | 62.5 % | 0 / 146 (alphabet has none) |
| PaddleOCR v3 det + v5 Latin rec | **0.20 s** | **0.2 %** | **235 / 236** |
| PaddleOCR v5 server det + v5 Latin rec | 1.38 s | 1.0 % | 233 / 236 |

Each finding below changed the result:
- **Deskew.** A tilted page needs it. Axis-aligned line boxes on a 3° tilt overlap and give 50–70 % CER. The fix measures the text's skew from the detected regions' second moments, straightens the image and detects again.
- **Crop padding (unclip) of 2.5.** At PaddleOCR's 1.5, the dots of ä ö ü and the tops of tall letters fall outside the crop: Muller, Offnungszeiten, "Fanrzeuge". 2.5 keeps tight 1.25× line spacing apart. At 3.0 the tight pages start to merge.
- **ocrs is ruled out for this household.** Its alphabet has no ä ö ü ß.

All of the above runs in pure Rust: rten 0.28 loads the ONNX files directly, so there is no ONNX Runtime and no Python.

## Results on a Raspberry Pi 5, 8 GB (smithy, 2026-11-19)

Setup: binary sha256 b31bbf96…, 4 threads, `nice -n 19`. Fonts were
DejaVu Sans and Serif, so the set was 12 pictures and 84 lines. lichtbild
was idle. The Pi reached 73.6 °C and did not throttle.

| engine | per picture (median, max) | CER | umlauts/ß right |
|---|---|---|---|
| PaddleOCR v3 det + v5 Latin rec | **1.02 s**, 1.45 s (tilted pages) | **0.0 %** | **125 / 125** |
| PaddleOCR v5 server det | 6.70 s, 12.91 s | 0.8 % | 124 / 125 |
| ocrs 0.14 | 7.94 s, 9.41 s | 69.1 % | 0 / 125 |

- The Pi is about 6.5× slower than the M4 for v3.
- A sign in a scene takes about 0.55 s. A deskewed page takes about 1.4 s, because detection runs twice.
- Peak RSS was about 0.9 GB for the whole process, with all three engines loaded one after another.
- At about 1 s per picture, about 31,000 photos take about 9 hours of 4-core CPU. That is a few nights at `nice -n 19`.

**Decision for v0.7:** PaddleOCR v3 det + v5 Latin rec, run by rten, in a child process after the scan, like transcription.
