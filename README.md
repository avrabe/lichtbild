# lichtbild

A self-hosted photo and video library for a household, as one Rust binary.

lichtbild indexes photo folders read-only, in place, without copying or moving them. It serves:

- a timeline per person, or merged;
- a world map of where photos and videos were taken;
- later, face recognition and similar search.

Everything runs on the home server. The map tiles come from a local file, and no photo, place or face leaves the house.

Status: v0.5.0. `lichtbild serve` indexes the configured folders read-only (JPEG, PNG, WebP, GIF and HEIC photos; MP4 and MOV videos with a poster frame and their length) and serves a timeline per person with a date scrubber, a viewer with swipe, arrow keys, zoom and full screen that plays videos, an info panel with offline town names, a world map from a self-hosted PMTiles file, transcripts of what is said in videos (Whisper, run in the house), one search box over speech, places, dates and names, duplicates shown once with per-person hiding and a cleanup list, and the house's nav switcher. With an OIDC issuer configured, everyone signs in through the house identity provider and sees their own library plus what they were granted; without one it stays on loopback for the operator. See [docs/BRIEF.md](docs/BRIEF.md) for what it should become.

## Use

```sh
export LICHTBILD_ROOTS='person1=/srv/photos/person1;person2=/srv/photos/person2'
export LICHTBILD_DATA_DIR=/var/lib/lichtbild
lichtbild serve    # http://127.0.0.1:8080/, scans at start and every 24 h
lichtbild scan     # one scan and its report, only while no server runs; exit 2 if found != indexed + skipped
```

The index is a redb file in the data directory, owned by one process: `serve` scans in a low-priority background thread and logs the report; `scan` refuses while a server holds the index.

All settings are in `lichtbild --help` and [config.example.toml](config.example.toml). The data directory must exist (lichtbild creates nothing at startup), and lichtbild refuses to start when it and a root overlap. Behind a reverse proxy, set `LICHTBILD_ALLOWED_HOSTS` to the public host name: requests for any other host get 421.

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
