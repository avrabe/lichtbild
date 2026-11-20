# Test fixtures

Synthetic or public domain with a recorded source (REQ-PRIV-001). No photo
of a place, and no person but public-domain portraits of public figures
for the face tests (owner's decision, 2026-11-20); nothing from the
household.

- `synthetic-640x480.heic`: a generated 640x480 RGB gradient with a
  checkerboard, written as PNG by a script and converted with macOS
  `sips -s format heic`. sips stores it as a 2x1 grid of 512x512 tiles, so
  it exercises heic-rs's grid path. 9,024 bytes.
- `places-cities.txt`, `places-admin1.txt`, `places-countries.txt`: made-up
  towns ("Ostdorf", "Westdorf", "Bergheim") in a made-up country "ZZ /
  Testland", in the GeoNames dump format, near the made-up coordinates the
  EXIF fixture carries. No real place.
- `test-oidc-rsa.pem`: an RSA key generated for these tests only. The mock
  identity provider in `tests/auth.rs` signs ID tokens with it. It protects
  nothing and is used nowhere else.
- `video/`: made by `python3 scripts/video_fixtures.py` from ffmpeg's
  `testsrc2` pattern and a sine tone, with made-up dates and the made-up
  coordinates above. The script moves two metadata boxes to where phones
  write them, because ffmpeg writes them elsewhere:
  - `synthetic-h264-160x120.mp4`: H.264 High with B-frames and AAC, UTC
    `mvhd` time and a `©xyz` position in `moov/udta`, as Android writes
    them.
  - `synthetic-hevc10-rot90.mov`: 10-bit HEVC (`hvc1`) stored 176x144 with
    the clockwise 90° matrix of an iPhone portrait clip, and the
    `com.apple.quicktime.*` creation date (with offset) and location in
    `moov/meta`.
  - `synthetic-mpeg4.mp4`: MPEG-4 Part 2, a codec lichtbild does not decode.
  - `truncated.mp4`: the first 3,000 bytes of the H.264 clip, with no `moov`.
  - `librivox-kafka-galerie-de.mp4`: 19.5 s of a public-domain LibriVox
    reading (Franz Kafka, "Auf der Galerie", read by Thorsten Herbold,
    librivox.org), from the title on, as mono AAC with a still frame. The
    script downloads the MP3 from archive.org and checks its SHA-256.
  - `synthetic-tone-silence.mp4`: 3 s of a 440 Hz tone, then 4 s of
    silence: no speech.
- The Whisper model for tests is not in the repository:
  `scripts/fetch-test-model.sh <dir>` downloads Whisper tiny q8_0 (MIT
  weights, converted by lmz/candle-whisper) at a pinned revision and checks
  each file's SHA-256; tests read it from `LICHTBILD_TEST_WHISPER_DIR`.
- `text/two-letters.png`: two sheets of paper on a table, one with a
  heading and three lines, one with two lines, in German with ä ö ü ß and
  €. The sentences were written for lichtbild. They are drawn with Atkinson
  Hyperlegible Next (SIL Open Font License 1.1) by a one-off program, for
  tests/ocr.rs.
- `vision/cc0-cat-on-sofa.jpg`: a black cat curled up on a sofa, by Salwa
  Farwaneh Dameh, from Wikimedia Commons ("Black cat curled up on a sofa",
  CC0 1.0, public domain dedication), scaled to 960 px and re-encoded so
  no EXIF or other metadata is kept; for tests/vision.rs.
- The vision models for tests are not in the repository either:
  `scripts/fetch-vision-models.sh <dir>` downloads PaddleOCR detection and
  Latin recognition, RT-DETRv2-R18 and SigLIP multilingual (all
  Apache-2.0, ONNX) at pinned revisions and checks each file's SHA-256.
  Tests read them from `LICHTBILD_TEST_VISION_DIR`.
- `faces/`: for tests/faces.rs, NASA photographs (works of the US
  government, public domain), from Wikimedia Commons, scaled (portraits to
  480 px, the group to 1024 px) and re-encoded so no EXIF or other
  metadata is kept. The file names say only who is the same person:
  - `pd-portrait-a1.jpg`: "Jessica Meir astronaut candidate.jpg" (NASA,
    https://commons.wikimedia.org/wiki/File:Jessica_Meir_astronaut_candidate.jpg);
  - `pd-portrait-a2.jpg`: "Jessica U. Meir portrait for media event.jpg"
    (NASA, Robert Markowitz,
    https://commons.wikimedia.org/wiki/File:Jessica_U._Meir_portrait_for_media_event.jpg);
  - `pd-portrait-b1.jpg`: "2013 NASA Astronaut Class - Christina Koch -
    (cropped).jpg" (NASA, James Blair,
    https://commons.wikimedia.org/wiki/File:2013_NASA_Astronaut_Class_-_Christina_Koch_-_(cropped).jpg);
  - `pd-group.jpg`: "2013 class of NASA astronauts.jpg", eight people,
    both of the above among them (NASA, Robert Markowitz,
    https://commons.wikimedia.org/wiki/File:2013_class_of_NASA_astronauts.jpg).
- The face models are fetched by `scripts/fetch-vision-models.sh` with
  the vision models: YuNet 2026may (opencv_zoo, MIT) and AuraFace v1
  (fal/AuraFace-v1, Apache-2.0, its glintr100.onnx only).
