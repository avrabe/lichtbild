#!/usr/bin/env python3
"""Generate the synthetic video fixtures in tests/fixtures/video.

Test patterns only (ffmpeg's testsrc2 and a sine tone), made-up dates and
the made-up coordinates of tests/fixtures/places-*.txt. ffmpeg does not
write metadata where phones do, so two boxes are moved afterwards:

- Android writes the position as moov/udta/©xyz; ffmpeg's MP4 muxer writes
  a 3GPP loci box instead. It is replaced by ©xyz.
- iPhones write the com.apple.quicktime.* keys in moov/meta; ffmpeg puts
  that meta box in moov/udta. It is moved up to moov.

Both files keep mdat before moov, so no chunk offset changes.

Usage: python3 scripts/video_fixtures.py   (needs ffmpeg with libx264 and
libx265 on PATH; run from the repository root)
"""

import os
import struct
import subprocess
import sys
import tempfile

OUT = os.path.join("tests", "fixtures", "video")


def run(*args):
    subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", *args], check=True)


def boxes(b, start, end):
    """(kind, start, end, header) for each box in b[start:end]."""
    out, at = [], start
    while at + 8 <= end:
        size, kind = struct.unpack(">I4s", b[at:at + 8])
        hdr = 8
        if size == 1:
            size, hdr = struct.unpack(">Q", b[at + 8:at + 16])[0], 16
        elif size == 0:
            size = end - at
        assert hdr <= size <= end - at, "malformed box"
        out.append((kind, at, at + size, hdr))
        at += size
    return out


def box(kind, body):
    return struct.pack(">I4s", 8 + len(body), kind) + body


def rewrite_moov(data, edit):
    """Replace the top-level moov (which must come last) with edit(children)."""
    top = boxes(data, 0, len(data))
    kind, s, e, h = top[-1]
    assert kind == b"moov", "moov must be the last box"
    kids = [(k, data[a:b], data[a + hh:b]) for k, a, b, hh in boxes(data, s + h, e)]
    return data[:s] + box(b"moov", b"".join(edit(kids)))


def android_xyz(kids, iso6709):
    out = []
    for kind, whole, body in kids:
        if kind != b"udta":
            out.append(whole)
            continue
        inner = [body[a:b] for k, a, b, _ in boxes(body, 0, len(body)) if k != b"loci"]
        text = iso6709.encode()
        xyz = box(b"\xa9xyz", struct.pack(">HH", len(text), 0x15C7) + text)
        out.append(box(b"udta", b"".join(inner) + xyz))
    return out


def apple_meta(kids):
    out, meta = [], None
    for kind, whole, body in kids:
        if kind != b"udta":
            out.append(whole)
            continue
        rest = []
        for k, a, b, _ in boxes(body, 0, len(body)):
            if k == b"meta":
                meta = body[a:b]
            else:
                rest.append(body[a:b])
        if rest:
            out.append(box(b"udta", b"".join(rest)))
    assert meta, "no udta/meta to move"
    return out + [meta]


def main():
    os.makedirs(OUT, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        t = lambda n: os.path.join(tmp, n)
        # H.264 High with B-frames and an AAC tone, as an Android phone
        # would write it: mvhd time in UTC, position in udta/©xyz.
        run("-f", "lavfi", "-i", "testsrc2=size=160x120:rate=25:duration=1",
            "-f", "lavfi", "-i", "sine=frequency=440:duration=1",
            "-c:v", "libx264", "-profile:v", "high", "-pix_fmt", "yuv420p",
            "-bf", "2", "-g", "25", "-c:a", "aac", "-b:a", "32k",
            "-map_metadata", "-1", "-fflags", "+bitexact",
            "-flags:v", "+bitexact", "-flags:a", "+bitexact",
            "-metadata", "creation_time=2024-05-06T07:08:09Z",
            "-metadata", "location=+10.5200+020.2700/", t("h264.mp4"))
        with open(t("h264.mp4"), "rb") as f:
            data = rewrite_moov(f.read(), lambda k: android_xyz(k, "+10.5200+020.2700/"))
        with open(os.path.join(OUT, "synthetic-h264-160x120.mp4"), "wb") as f:
            f.write(data)

        # 10-bit HEVC stored landscape with the clockwise 90-degree display
        # matrix of an iPhone portrait clip (ffmpeg's -90): Apple keys in
        # moov/meta.
        run("-f", "lavfi", "-i", "testsrc2=size=176x144:rate=25:duration=1",
            "-c:v", "libx265", "-x265-params", "log-level=error",
            "-pix_fmt", "yuv420p10le", "-tag:v", "hvc1", "-g", "25",
            "-map_metadata", "-1", t("hevc-plain.mov"))
        run("-display_rotation", "-90", "-i", t("hevc-plain.mov"), "-c", "copy",
            "-tag:v", "hvc1", "-movflags", "use_metadata_tags", "-map_metadata", "-1",
            "-metadata", "creation_time=2024-07-08T19:30:00Z",
            "-metadata", "com.apple.quicktime.creationdate=2024-07-08T21:30:00+0200",
            "-metadata", "com.apple.quicktime.location.ISO6709=+11.3000+020.1000+100.000/",
            t("hevc.mov"))
        with open(t("hevc.mov"), "rb") as f:
            data = rewrite_moov(f.read(), apple_meta)
        with open(os.path.join(OUT, "synthetic-hevc10-rot90.mov"), "wb") as f:
            f.write(data)

    # Speech for transcripts (REQ-MEDIA-004): 19.5 s of a public-domain
    # LibriVox reading (Kafka, "Auf der Galerie", read by Thorsten
    # Herbold; librivox.org, public domain), from the title on, as AAC mono
    # in an MP4 with a still frame. Downloaded and checked by SHA-256.
    kafka = os.path.join(tempfile.gettempdir(), "lichtbild-kafka-galerie.mp3")
    if not os.path.isfile(kafka):
        subprocess.run(["curl", "-sfL", "-o", kafka,
                        "https://archive.org/download/kafka_galerie_librivox/auf_der_galerie_kafka_64kb.mp3"],
                       check=True)
    import hashlib
    with open(kafka, "rb") as f:
        assert hashlib.sha256(f.read()).hexdigest() == \
            "6b658c94403a972c51c8d192ecdfd4e9a3ad86ecf154bfa8341ac32f6a572edf", "kafka mp3 changed"
    run("-ss", "15.5", "-t", "19.5", "-i", kafka,
        "-f", "lavfi", "-i", "color=c=0x334455:s=64x48:r=5",
        "-map", "1:v", "-map", "0:a", "-shortest",
        "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-ac", "1", "-ar", "16000", "-b:a", "24k",
        "-map_metadata", "-1", "-fflags", "+bitexact", "-flags:v", "+bitexact", "-flags:a", "+bitexact",
        os.path.join(OUT, "librivox-kafka-galerie-de.mp4"))
    # No speech: 3 s of a 440 Hz tone, then 4 s of silence.
    run("-f", "lavfi", "-i", "sine=frequency=440:duration=3",
        "-f", "lavfi", "-i", "anullsrc=r=16000:cl=mono:d=4",
        "-f", "lavfi", "-i", "color=c=gray:s=64x48:r=5:d=7",
        "-filter_complex", "[0:a]aresample=16000[a0];[a0][1:a]concat=n=2:v=0:a=1[a]",
        "-map", "2:v", "-map", "[a]", "-c:v", "libx264", "-pix_fmt", "yuv420p",
        "-c:a", "aac", "-ac", "1", "-b:a", "24k", "-map_metadata", "-1",
        "-fflags", "+bitexact", "-flags:v", "+bitexact", "-flags:a", "+bitexact",
        os.path.join(OUT, "synthetic-tone-silence.mp4"))

    # MPEG-4 Part 2, a codec lichtbild does not decode.
    run("-f", "lavfi", "-i", "testsrc2=size=160x120:rate=25:duration=1",
        "-c:v", "mpeg4", "-q:v", "31", "-map_metadata", "-1",
        os.path.join(OUT, "synthetic-mpeg4.mp4"))
    # The first 3000 bytes of the H.264 clip: no moov.
    with open(os.path.join(OUT, "synthetic-h264-160x120.mp4"), "rb") as f:
        head = f.read(3000)
    with open(os.path.join(OUT, "truncated.mp4"), "wb") as f:
        f.write(head)


if __name__ == "__main__":
    sys.exit(main())
