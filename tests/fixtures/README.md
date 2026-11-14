# Test fixtures

Synthetic only (REQ-PRIV-001). No photo of a person or place.

- `synthetic-640x480.heic`: a generated 640x480 RGB gradient with a
  checkerboard, written as PNG by a script and converted with macOS
  `sips -s format heic`. sips stores it as a 2x1 grid of 512x512 tiles, so
  it exercises heic-rs's grid path. 9,024 bytes.
