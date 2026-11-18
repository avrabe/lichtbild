# Test fixtures

Synthetic only (REQ-PRIV-001). No photo of a person or place.

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
