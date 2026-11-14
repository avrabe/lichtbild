# lichtbild: first brief

Owner decision, 2026-11-14. lichtbild is the household's own photo app. It
follows the haus and warte style: one Rust binary, server-rendered pages
(htmx), static assets embedded, a shared nav, login through the house's OIDC
provider, and verification and traceability with rivet from commit one.

## What it does

- **Read-only index of existing folders.** One root folder per person, given
  in the config. Each holds the files a nightly cloud copy puts there. lichtbild
  never writes, moves or deletes inside them. Thumbnails, previews, the index
  database and model output live in lichtbild's own data directory.
- **Photos and videos.** Common photo formats, including HEIC and Motion
  Photos, plus video (MP4/MOV), with poster frames and streaming playback.
- **Timeline.** Grouped by day, month and year, per person or merged, with
  fast scrolling through tens of thousands of items. The first target is
  about 31,000 files, 165 GiB.
- **World map.** Photos placed by their GPS EXIF, clustered by zoom. Tiles
  come from a single self-hosted PMTiles file (Protomaps), served by
  lichtbild itself. The browser makes no request to an outside tile server.
- **Later:** face recognition with grouping into people, similar-image
  search, and places by name. All models run locally (for example ONNX
  through the `ort` crate). No cloud API, and model files are pinned by
  sha256.

## How it runs

- Target: a Raspberry Pi 5 (8 GB, arm64), with the library and data on NVMe.
  Indexing runs at low priority with bounded parallelism. The first full
  index may take hours and must survive restarts (resumable, idempotent).
- One process, one systemd unit, behind the house reverse proxy under a path
  prefix (base path configurable, like haus's HAUS_BASE_PATH).
- Configuration from environment and files. Private values (folder layout,
  OIDC client secret, location rounding) come from the deployment, never
  from this repository.
- Login: OIDC against the house identity provider. Each person sees their
  own timeline; the merged view and sharing are explicit.
- Nav v1: renders the shared app switcher file the same way haus and warte
  do (contract in `warte docs nav`).

## Rules this repo holds itself to

- No private data in the repo, issues or PRs: no photos, places, names,
  hostnames, addresses or account details. Test fixtures are synthetic or
  public-domain.
- Every claimed property has a check that can fail. Plant faults to prove
  each one.
- Self-hosted CI jobs use the fail-closed fork guard:
  `(github.event_name != 'pull_request' || github.event.pull_request.head.repo.full_name == github.repository) && <self-hosted labels> || 'ubuntu-latest'`.
  No self-hosted job sits in a `pull_request_target` workflow.
- Releases are signed (cosign), with sha256 sums and attestations, so the
  deployment can pin them like haus.

## First steps

1. Bootstrap verification (rivet): losses and hazards first. Examples:
   writing into a source folder; a person seeing the other's private items;
   a location leaking to an outside service; an index that silently skips
   files. Then requirements linked to them.
2. A skeleton binary: config, read-only scan with a report of files found
   and skipped, a redb index (owner decision 2026-11-14: redb, no SQLite),
   a thumbnail cache, and a timeline page.
3. OIDC login, per-person views and nav, then the map with PMTiles, then
   video, then faces. (Owner decision 2026-11-14: login moves ahead of the
   map, because nothing is exposed to the household before it exists.
   v0.1.0 is deployed for the operator only, to measure the real library.)
