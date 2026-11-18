# Vendored map libraries

The world map (REQ-MAP-001, DD-017) runs on these files, embedded in the
binary like htmx and served from `static/` by lichtbild itself. They are
byte-for-byte the files of the npm releases named below: nothing is
modified, so each sha256 can be checked against the package. The test
`vendored_map_assets_match_their_pinned_sha256` in `tests/map.rs` fails if
an embedded file differs from the hash pinned here.

| File in this directory | Package and version | Licence | sha256 |
|---|---|---|---|
| `maplibre-gl-5.24.1/maplibre-gl-csp.js` | `maplibre-gl` 5.24.1, `dist/` | BSD-3-Clause (`maplibre-gl-5.24.1/LICENSE.txt`) | `2835432eb59578c24e9bc7fbcef6f6f03f2af3f60951b940db875125ff617ddb` |
| `maplibre-gl-5.24.1/maplibre-gl-csp-worker.js` | `maplibre-gl` 5.24.1, `dist/` | BSD-3-Clause | `58534d0ebbe8535cd22d19eb3739788898667390ab9b22b87edfe55b9c85d0a5` |
| `maplibre-gl-5.24.1/maplibre-gl.css` | `maplibre-gl` 5.24.1, `dist/` | BSD-3-Clause | `ab1e70d59ec40465bae7e7030da2f3ccf28133fd502e62bd598eefbadfd7a732` |
| `pmtiles-4.5.0/pmtiles.js` | `pmtiles` 4.5.0, `dist/` | BSD-3-Clause (`pmtiles-4.5.0/LICENSE`) | `caf981bc46f6327ee7e65d5dc964d89d38a69f60edca2bd4c5c890c21b554c6c` |
| `protomaps-basemaps-5.7.2/basemaps.js` | `@protomaps/basemaps` 5.7.2, `dist/` | BSD-3-Clause code, CC0 visual design (`protomaps-basemaps-5.7.2/LICENSE.md`) | `551864f01da1bfe3005e99987197f22f56d0c35f9a10fa1727a889e2434af16d` |

Where they came from, and how they were checked (2026-11-18):

- Each tarball was downloaded from registry.npmjs.org and matched the
  registry's `sha512` integrity value.
- The three MapLibre files also match the `dist.zip` asset of the immutable
  GitHub release `v5.24.1` of maplibre/maplibre-gl-js.
- 5.24.1 is the maintained 5.x line (a backport of attribution-sanitising
  fixes). MapLibre 6 ships ES modules only and no CSP bundle; it was not
  taken.
- `LICENSE.txt` is the one in the maplibre-gl package; `pmtiles-4.5.0/LICENSE`
  and `protomaps-basemaps-5.7.2/LICENSE.md` are copied from the
  protomaps/PMTiles and protomaps/basemaps repositories, since the npm
  packages ship none.

Why the CSP build: the standard MapLibre bundle starts its web worker from a
`blob:` URL, which a same-origin CSP blocks. `maplibre-gl-csp.js` loads its
worker from `maplibre-gl-csp-worker.js` instead, a file lichtbild serves
itself (`maplibregl.setWorkerUrl` in `assets/map.js`), so the CSP stays
`'self'` only, with no `blob:`, `worker-src` or `unsafe-eval`.

`maplibre-gl.css` draws its control icons as `data:` SVG images, which
`img-src 'self'` blocks; `lichtbild.css` replaces those few icons with text
so the browser never tries to load them.

## Map data

The tiles are not vendored. They come from one PMTiles file the deployment
provides (`LICHTBILD_MAP_PMTILES`), cut from the Protomaps basemap build,
which is derived from OpenStreetMap (ODbL) and Natural Earth (public domain).
The map shows "© OpenStreetMap" as required by the ODbL. Fonts (SIL Open
Font License) and sprites (derived from MIT-licensed tangrams/icons) come
from the deployment's copy of protomaps/basemaps-assets
(`LICHTBILD_MAP_ASSETS`).

## Updating

Download the new tarballs, check their integrity against the registry,
copy the same `dist/` files into a new versioned directory, update the file
names in `src/map.rs` and this table, and run `cargo test --test map`.
