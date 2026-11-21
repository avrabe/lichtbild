# lichtbild

A self-hosted photo and video library for a household, as one Rust binary.

lichtbild indexes photo folders read-only, in place, without copying or moving them. It serves:

- a timeline per person, or merged, with bursts folded into one tile and a slideshow;
- favourites and albums, each person their own (an album can be shared with the house), with a slideshow of each;
- a world map of where photos and videos were taken, and trips found from it;
- one search box over what was said in videos, the text in pictures, the things in them (Katze, Fahrrad), places, dates, seasons and holidays, or a description in your own words, with suggestions as you type;
- the people in the photos: faces grouped by person, named once for the whole house, found by name in search; each person sees only their own photos of anyone.
- corrections where the mistake shows: a thing found that is not there, another kind, a kind never in the house, a line misread; they hold for the house, survive the next look at the photos, raise the bar for a kind that keeps being wrong, and download as a file to teach a model one day.

Everything runs on the home server. The map tiles come from a local file, and no photo, place or face leaves the house.

Status: v0.11.0. `lichtbild serve` indexes the configured folders read-only (JPEG, PNG, WebP, GIF and HEIC photos; MP4 and MOV videos with a poster frame and their length) and serves a timeline per person with a date scrubber, a viewer with swipe, arrow keys, zoom and full screen that plays videos, an info panel with offline town names, a world map from a self-hosted PMTiles file, transcripts of what is said in videos (Whisper, run in the house), one search box over speech, places, dates and names, duplicates shown once with per-person hiding and a cleanup list, a photo/video filter, a cloud of the words said in videos, text, things and search by meaning in photos, estimated places for photos without a position, series of look-alike shots, faces gathered into people the house names together (each person sees a name only through their own photos), corrections the house makes to wrong text and things (in its own words too), which then hold everywhere, a slideshow (also of chosen pictures), trips and single days shown on their own with the days before and after, the best shot leading each series, the countries visited, the map narrowed to a trip or a range of months, videos with their transcript as captions, a day or night look, a status page with the work as steps and how quickly pages answer, the countries visited shaded on the map, relations between people with the close ones first, strangers and people hidden in one tap, a made-up story or poem of a day or trip, slideshows on the TV through AirPlay, favourites and albums per person, a status page for the scan and transcription, one menu on every page, and the house's apps in one menu at the top right. With an OIDC issuer configured, everyone signs in through the house identity provider and sees their own library plus what they were granted; without one it stays on loopback for the operator. See [docs/BRIEF.md](docs/BRIEF.md) for what it should become.

## Use

```sh
export LICHTBILD_ROOTS='person1=/srv/photos/person1;person2=/srv/photos/person2'
export LICHTBILD_DATA_DIR=/var/lib/lichtbild
lichtbild serve    # http://127.0.0.1:8080/, scans at start and every 24 h
lichtbild scan     # one scan and its report, only while no server runs; exit 2 if found != indexed + skipped
```

The index is a redb file in the data directory, owned by one process: `serve` scans in a low-priority background thread and logs the report; `scan` refuses while a server holds the index.

All settings are in `lichtbild --help` and [config.example.toml](config.example.toml). The data directory must exist (lichtbild creates nothing at startup), and lichtbild refuses to start when it and a root overlap. Behind a reverse proxy, set `LICHTBILD_ALLOWED_HOSTS` to the public host name: requests for any other host get 421.

### Looking at photos

With `LICHTBILD_VISION_DIR` set, every photo is looked at once in a background child process after the scan: the text in it (PaddleOCR), the things in it (RT-DETRv2, 80 kinds) and a picture vector for search by meaning and look-alikes (SigLIP, multilingual). `scripts/fetch-vision-models.sh <dir>` fetches the seven pinned files (about 410 MB) and picks compact int8 models where the CPU runs them correctly, float32 elsewhere; lichtbild refuses a set that does not match the CPU. On a Raspberry Pi 5 a photo takes about 2 s. Searching by meaning loads a text encoder of about 0.6 GB, dropped again after 10 idle minutes. The same directory may hold the two face models (YuNet and AuraFace, about 250 MB, fetched by the same script unless `LICHTBILD_FACES=0`): then every photo is also looked at for faces, once, after that pass, and the People page groups them for a name.

## Verification

Losses, hazards, constraints, requirements and tests are rivet artifacts in [artifacts/](artifacts/); the control structure is in [docs/control-structure.md](docs/control-structure.md). `varve run -- rivet validate` must pass with 0 warnings. [docs/planted-faults.md](docs/planted-faults.md) lists the fault that shows each check can fail.

Releases are signed like haus's: verify `SHA256SUMS.txt` with

```sh
cosign verify-blob \
  --certificate-identity-regexp '^https://github\.com/avrabe/lichtbild/\.github/workflows/release\.yml@refs/tags/v[0-9][^/]*$' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --bundle SHA256SUMS.txt.cosign.bundle SHA256SUMS.txt
sha256sum -c --ignore-missing SHA256SUMS.txt
```

## License

Apache-2.0, see [LICENSE](LICENSE).
