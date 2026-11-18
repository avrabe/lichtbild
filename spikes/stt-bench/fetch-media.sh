#!/bin/sh
# Open test media for stt-bench, with reference texts. Run in an empty dir.
# - Tears of Steel (Blender Foundation, CC BY 3.0, mango.blender.org):
#   English dialogue over music and effects; subtitles as reference.
# - LibriVox recordings (public domain), German:
#   Kafka, "Auf der Galerie" (read by Thorsten Herbold); reference text
#   from de.wikisource (public domain). Grimm, "Die Sterntaler" (speed only).
set -eu
get() { curl -sfL -A stt-bench -o "$1" "$2"; }
get tears_of_steel_720p.mov https://download.blender.org/demo/movies/ToS/tears_of_steel_720p.mov
get TOS-en.srt https://download.blender.org/demo/movies/ToS/subtitles/TOS-en.srt
get kafka_galerie.mp3 https://archive.org/download/kafka_galerie_librivox/auf_der_galerie_kafka_64kb.mp3
get grimm_sterntaler.mp3 https://archive.org/download/grimm_maerchen_1_librivox/grimm_132_sterntaler_64kb.mp3
get galerie.html "https://de.wikisource.org/w/index.php?title=Auf_der_Galerie&action=render"
python3 -I - <<'PY'
import re, html
h = open('galerie.html', encoding='utf-8').read()
h = re.sub(r'(?s)<table.*?</table>|<(script|style).*?</\2>', '', h)
t = html.unescape(re.sub(r'<[^>]+>', ' ', h))
open('galerie.txt', 'w').write(re.sub(r'\s+', ' ', t).strip())
PY
shasum -a 256 -c <<'SUMS'
efa9062d9cdb7a338e40ad530dfdf234806743f29ae6a1a136b97ece4e588e8f  tears_of_steel_720p.mov
6b658c94403a972c51c8d192ecdfd4e9a3ad86ecf154bfa8341ac32f6a572edf  kafka_galerie.mp3
6d328c9f8a2ab7d7e1e3d0d31e3f60306eb7eaefd8e55e41026a7661e94f3b7d  grimm_sterntaler.mp3
SUMS
