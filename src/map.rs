//! The world map (REQ-MAP-001, DD-017): a page with MapLibre over one
//! self-hosted Protomaps PMTiles file, and photo clusters computed here.
//!
//! - Tiles: the deployment's PMTiles file, served read-only with byte
//!   ranges at `map/tiles.pmtiles`. Nothing else is read through that route.
//! - Glyphs and sprites: the deployment's assets directory, served at
//!   `map/assets/...` with the path confined to that directory exactly as a
//!   root is confined (`source::resolve`, SSC-004).
//! - Clusters: `map/clusters` aggregates the live items with a position on a
//!   Web Mercator grid per zoom, scoped to the viewer like the timeline
//!   (REQ-AUTH-002): a point whose library the viewer may not see is never
//!   counted, placed or listed.
//! - The browser loads the vendored libraries from `static/` and nothing
//!   from another host (SSC-003); see assets/VENDORED.md.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Extension;
use serde::{Deserialize, Serialize};

use crate::auth::Viewer;
use crate::config::MapFiles;
use crate::index::Index;
use crate::web::{esc, AppState};

/// The vendored files, served from `static/` under these names. Each name
/// carries its upstream version, so a browser never mixes two versions.
pub const MAPLIBRE_JS: (&str, &str) = (
    "maplibre-gl-csp-5.24.1.js",
    include_str!("../assets/maplibre-gl-5.24.1/maplibre-gl-csp.js"),
);
pub const MAPLIBRE_WORKER: (&str, &str) = (
    "maplibre-gl-csp-worker-5.24.1.js",
    include_str!("../assets/maplibre-gl-5.24.1/maplibre-gl-csp-worker.js"),
);
pub const MAPLIBRE_CSS: (&str, &str) = (
    "maplibre-gl-5.24.1.css",
    include_str!("../assets/maplibre-gl-5.24.1/maplibre-gl.css"),
);
pub const PMTILES_JS: (&str, &str) = (
    "pmtiles-4.5.0.js",
    include_str!("../assets/pmtiles-4.5.0/pmtiles.js"),
);
pub const BASEMAPS_JS: (&str, &str) = (
    "basemaps-5.7.2.js",
    include_str!("../assets/protomaps-basemaps-5.7.2/basemaps.js"),
);
/// lichtbild's own map script (not vendored).
pub const MAP_JS: &str = include_str!("../assets/map.js");

/// The vendored files with their served names, for `static/` and the tests.
pub fn vendored() -> [(&'static str, &'static str); 5] {
    [
        MAPLIBRE_JS,
        MAPLIBRE_WORKER,
        MAPLIBRE_CSS,
        PMTILES_JS,
        BASEMAPS_JS,
    ]
}

/// A static file of the map: (body, content type).
pub fn static_asset(name: &str) -> Option<(&'static str, &'static str)> {
    if name == "map.js" {
        return Some((MAP_JS, "text/javascript; charset=utf-8"));
    }
    vendored()
        .into_iter()
        .find(|(n, _)| *n == name)
        .map(|(n, body)| {
            let ct = if n.ends_with(".css") {
                "text/css; charset=utf-8"
            } else {
                "text/javascript; charset=utf-8"
            };
            (body, ct)
        })
}

/// The first bytes of every PMTiles v3 archive.
const PMTILES_MAGIC: &[u8] = b"PMTiles\x03";

/// Validate the configured map files at startup: absolute, existing, a
/// PMTiles v3 file and a directory. Returns the canonical paths, so the
/// assets directory can confine requests like a root does.
pub fn check_files(m: &MapFiles) -> Result<MapFiles> {
    let pmtiles = match &m.pmtiles {
        None => None,
        Some(p) => {
            if !p.is_absolute() {
                bail!("LICHTBILD_MAP_PMTILES must be an absolute path");
            }
            let c = p
                .canonicalize()
                .context("LICHTBILD_MAP_PMTILES: cannot open the file")?;
            let mut magic = [0u8; 8];
            std::fs::File::open(&c)
                .and_then(|f| f.take(8).read_exact(&mut magic))
                .context("LICHTBILD_MAP_PMTILES: cannot read the file")?;
            if magic != PMTILES_MAGIC {
                bail!("LICHTBILD_MAP_PMTILES: not a PMTiles v3 file");
            }
            Some(c)
        }
    };
    let assets = match &m.assets {
        None => None,
        Some(p) => {
            if !p.is_absolute() {
                bail!("LICHTBILD_MAP_ASSETS must be an absolute path");
            }
            let c = p
                .canonicalize()
                .context("LICHTBILD_MAP_ASSETS: cannot open the directory")?;
            if !c.is_dir() {
                bail!("LICHTBILD_MAP_ASSETS: not a directory");
            }
            Some(c)
        }
    };
    Ok(MapFiles { pmtiles, assets })
}

/// One live item with a position.
#[derive(Debug, Clone)]
pub struct Point {
    pub id: u64,
    pub label: Arc<str>,
    pub lat: f64,
    pub lon: f64,
    /// Web Mercator, both in 0..1 (x east, y south).
    pub x: f64,
    pub y: f64,
    pub taken: String,
    pub thumb: Option<String>,
    pub video: bool,
}

/// Latitude limit of the Web Mercator square.
const MAX_LAT: f64 = 85.051_128_78;

pub fn mercator(lat: f64, lon: f64) -> (f64, f64) {
    let x = (lon + 180.0) / 360.0;
    let phi = lat.clamp(-MAX_LAT, MAX_LAT).to_radians();
    let y = (1.0 - (phi.tan() + 1.0 / phi.cos()).ln() / std::f64::consts::PI) / 2.0;
    (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0))
}

/// Every live item with a valid position, from the index.
pub fn points_from(idx: &Index) -> Result<Vec<Point>> {
    let mut labels: HashMap<String, Arc<str>> = HashMap::new();
    let mut out = Vec::new();
    for (id, rec) in idx.all()? {
        if rec.missing {
            continue;
        }
        let (Some(lat), Some(lon)) = (rec.lat, rec.lon) else {
            continue;
        };
        if !(lat.is_finite() && lon.is_finite() && lat.abs() <= 90.0 && lon.abs() <= 180.0) {
            continue;
        }
        let label = labels
            .entry(rec.label.clone())
            .or_insert_with(|| Arc::from(rec.label.as_str()))
            .clone();
        let (x, y) = mercator(lat, lon);
        out.push(Point {
            id,
            label,
            lat,
            lon,
            x,
            y,
            taken: rec.taken_local,
            thumb: (rec.thumb == "ok")
                .then(|| format!("thumb/{id}?h={}", rec.hash.get(..12).unwrap_or(""))),
            video: rec.video.is_some(),
        });
    }
    Ok(out)
}

/// The map's files and the positions cache, shared by all requests.
pub struct MapState {
    pub pmtiles: Option<PathBuf>,
    pub assets: Option<PathBuf>,
    /// The assets directory has `fonts/` (labels are drawn).
    pub glyphs: bool,
    /// The assets directory has `sprites/v4/` (icons are drawn).
    pub sprites: bool,
    cache: Mutex<Option<(u64, Arc<Vec<Point>>)>>,
}

impl MapState {
    pub fn new(files: &MapFiles) -> MapState {
        // Containment compares against the canonical directory; config
        // already resolved it, this keeps hand-built configs (tests) right.
        let assets = files
            .assets
            .as_ref()
            .map(|d| d.canonicalize().unwrap_or_else(|_| d.clone()));
        let has = |rel: &str| assets.as_ref().is_some_and(|d| d.join(rel).exists());
        MapState {
            pmtiles: files.pmtiles.clone(),
            glyphs: has("fonts"),
            sprites: has("sprites/v4/light.json"),
            assets,
            cache: Mutex::new(None),
        }
    }

    /// All positioned items, rebuilt when the index changed since the last
    /// call. Scoping happens per request, never in the cache.
    pub fn points(&self, idx: &Index) -> Result<Arc<Vec<Point>>> {
        let generation = idx.generation();
        if let Some((g, p)) = &*self.cache.lock().unwrap_or_else(|e| e.into_inner()) {
            if *g == generation {
                return Ok(p.clone());
            }
        }
        let p = Arc::new(points_from(idx)?);
        *self.cache.lock().unwrap_or_else(|e| e.into_inner()) = Some((generation, p.clone()));
        Ok(p)
    }
}

/// A bounding box in degrees: west, south, east, north. West may be greater
/// than east when the box crosses the antimeridian.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bbox {
    w: f64,
    s: f64,
    e: f64,
    n: f64,
    all_lons: bool,
}

/// "w,s,e,n" as MapLibre's getBounds gives it (longitudes may run past
/// ±180 when the world repeats).
pub fn parse_bbox(v: &str) -> Option<Bbox> {
    let f: Vec<f64> = v
        .split(',')
        .map(|x| x.trim().parse::<f64>().ok().filter(|x| x.is_finite()))
        .collect::<Option<_>>()?;
    let [w, s, e, n] = f.as_slice() else {
        return None;
    };
    if s > n || e < w {
        return None;
    }
    let wrap = |x: f64| (x + 180.0).rem_euclid(360.0) - 180.0;
    Some(Bbox {
        w: wrap(*w),
        s: s.max(-90.0),
        e: wrap(*e),
        n: n.min(90.0),
        all_lons: e - w >= 360.0,
    })
}

impl Bbox {
    pub fn contains(&self, lat: f64, lon: f64) -> bool {
        if lat < self.s || lat > self.n {
            return false;
        }
        if self.all_lons {
            return true;
        }
        if self.w <= self.e {
            (self.w..=self.e).contains(&lon)
        } else {
            lon >= self.w || lon <= self.e
        }
    }
}

/// Highest zoom with its own grid; above it the cells stay this small.
pub const MAX_ZOOM: u8 = 20;
/// From this zoom on, every cluster lists its newest items.
pub const DETAIL_ZOOM: u8 = 16;
/// Clusters of at most this many items list them all.
pub const ITEMS_MAX: usize = 12;
/// At most this many clusters per response (the largest are kept).
pub const CLUSTERS_MAX: usize = 4000;

#[derive(Debug, Serialize)]
pub struct MapItem {
    pub id: u64,
    /// The item's library: the viewer opens it at `v/{who}/{id}`.
    pub who: String,
    pub taken: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumb: Option<String>,
    pub video: bool,
}

#[derive(Debug, Serialize)]
pub struct Cluster {
    pub lat: f64,
    pub lon: f64,
    pub n: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<MapItem>,
}

#[derive(Debug, Serialize)]
pub struct Clusters {
    /// Positioned items the viewer may see, inside the box or not.
    pub total: usize,
    /// West, south, east, north of those items.
    pub bounds: Option<[f64; 4]>,
    pub clusters: Vec<Cluster>,
}

/// Aggregate the points of `labels` inside `bbox` on a grid of 64-pixel
/// cells at zoom `z` (2^(z+2) cells per side of the Mercator square).
pub fn clusters(points: &[Point], labels: &[String], z: u8, bbox: Option<Bbox>) -> Clusters {
    let z = z.min(MAX_ZOOM);
    let side = 1u64 << (u32::from(z) + 2);
    let cell = |v: f64| ((v * side as f64) as u64).min(side - 1);
    let mut total = 0;
    let mut bounds: Option<[f64; 4]> = None;
    // cell -> (sum lat, sum lon, members)
    let mut cells: HashMap<(u64, u64), (f64, f64, Vec<usize>)> = HashMap::new();
    for (i, p) in points.iter().enumerate() {
        // REQ-AUTH-002: only libraries the viewer may see.
        if !labels.iter().any(|l| l.as_str() == &*p.label) {
            continue;
        }
        total += 1;
        bounds = Some(match bounds {
            None => [p.lon, p.lat, p.lon, p.lat],
            Some([w, s, e, n]) => [w.min(p.lon), s.min(p.lat), e.max(p.lon), n.max(p.lat)],
        });
        if bbox.is_some_and(|b| !b.contains(p.lat, p.lon)) {
            continue;
        }
        let c = cells.entry((cell(p.x), cell(p.y))).or_default();
        c.0 += p.lat;
        c.1 += p.lon;
        c.2.push(i);
    }
    let mut out: Vec<Cluster> = cells
        .into_values()
        .map(|(lat, lon, mut members)| {
            let n = members.len();
            let items = if n <= ITEMS_MAX || z >= DETAIL_ZOOM {
                members.sort_by(|a, b| {
                    let (a, b) = (&points[*a], &points[*b]);
                    b.taken.cmp(&a.taken).then(b.id.cmp(&a.id))
                });
                members
                    .iter()
                    .take(ITEMS_MAX)
                    .map(|&i| {
                        let p = &points[i];
                        MapItem {
                            id: p.id,
                            who: p.label.to_string(),
                            taken: p.taken.clone(),
                            thumb: p.thumb.clone(),
                            video: p.video,
                        }
                    })
                    .collect()
            } else {
                Vec::new()
            };
            Cluster {
                lat: lat / n as f64,
                lon: lon / n as f64,
                n,
                items,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.n.cmp(&a.n)
            .then(a.lat.total_cmp(&b.lat))
            .then(a.lon.total_cmp(&b.lon))
    });
    out.truncate(CLUSTERS_MAX);
    Clusters {
        total,
        bounds,
        clusters: out,
    }
}

/// The libraries a map request covers: `who` like a timeline, else every
/// library the viewer may see.
fn map_scope(viewer: &Viewer, who: Option<&str>) -> Option<Vec<String>> {
    match who {
        Some(w) => crate::web::scope(viewer, w),
        None => Some(viewer.labels.clone()),
    }
}

#[derive(Deserialize)]
pub struct PageQuery {
    who: Option<String>,
    kind: Option<String>,
}

/// GET /map: the map, or a notice when no PMTiles file is configured.
pub async fn page(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    Query(q): Query<PageQuery>,
) -> Response {
    let Some(labels) = map_scope(&viewer, q.who.as_deref()) else {
        return (StatusCode::NOT_FOUND, "no such library").into_response();
    };
    let idx = st.index.clone();
    let map = st.map.clone();
    let res = tokio::task::spawn_blocking(move || {
        let points = map.points(&idx)?;
        anyhow::Ok(clusters(&points, &labels, 0, None))
    })
    .await;
    let summary = match res {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return crate::web::internal(e),
        Err(e) => return crate::web::internal(e),
    };
    let count = match summary.total {
        1 => "1 photo or video has a position.".to_string(),
        n => format!("{n} photos and videos have a position."),
    };
    let bar = toolbar(
        &viewer,
        q.who.as_deref(),
        crate::web::kind_filter(q.kind.as_deref()).0,
    );
    if st.map.pmtiles.is_none() {
        let body = format!(
            "{bar}<p class=\"mapnote\">No map file configured. The operator sets \
             <code>LICHTBILD_MAP_PMTILES</code> to a Protomaps PMTiles file.</p>\n<p>{count}</p>\n"
        );
        return crate::web::page_with_head(&st, "Map", "", &body).into_response();
    }
    let v = crate::web::asset_version();
    let head = format!(
        "<link rel=\"stylesheet\" href=\"static/{css}\">\n\
         <script src=\"static/{js}\" defer></script>\n\
         <script src=\"static/{pm}\" defer></script>\n\
         <script src=\"static/{bm}\" defer></script>\n\
         <script src=\"static/map.js?v={v}\" defer></script>\n",
        css = MAPLIBRE_CSS.0,
        js = MAPLIBRE_JS.0,
        pm = PMTILES_JS.0,
        bm = BASEMAPS_JS.0,
    );
    let mut attrs = format!(
        " data-pmtiles=\"map/tiles.pmtiles\" data-worker=\"static/{}\"",
        MAPLIBRE_WORKER.0
    );
    if st.map.glyphs {
        attrs += " data-glyphs=\"map/assets/fonts/\"";
    }
    if st.map.sprites {
        attrs += " data-sprite=\"map/assets/sprites/v4/\"";
    }
    if let Some(w) = &q.who {
        attrs += &format!(" data-who=\"{}\"", esc(w));
    }
    let (kind, _) = crate::web::kind_filter(q.kind.as_deref());
    match kind {
        Some(true) => attrs += " data-kind=\"video\"",
        Some(false) => attrs += " data-kind=\"photo\"",
        None => {}
    }
    if let Some([w, s, e, n]) = summary.bounds {
        attrs += &format!(" data-bounds=\"{w:.6},{s:.6},{e:.6},{n:.6}\"");
    }
    let body = format!(
        "<div class=\"lb-map\">\n{bar}<div id=\"map\" class=\"map\"{attrs}></div>\n\
         <div class=\"mapstrip\" id=\"mapstrip\" hidden></div>\n\
         <p class=\"mapnote\">{count} Map data © OpenStreetMap contributors.</p>\n</div>\n"
    );
    crate::web::page_with_head(&st, "Map", &head, &body).into_response()
}

/// The map's toolbar: whose pictures (a chip per library the viewer may
/// see, and all of them), and photos, videos or both.
fn toolbar(viewer: &Viewer, who: Option<&str>, kind: Option<bool>) -> String {
    let kq = match kind {
        Some(true) => "&amp;kind=video",
        Some(false) => "&amp;kind=photo",
        None => "",
    };
    let mut libs = String::new();
    if viewer.labels.len() > 1 {
        let chip = |label: &str, href: String, on: bool| {
            format!(
                "<a href=\"{href}\"{}>{}</a>",
                if on {
                    " class=\"on\" aria-current=\"true\""
                } else {
                    ""
                },
                esc(label)
            )
        };
        libs += "<nav class=\"kinds libs\" aria-label=\"Whose\">";
        libs += &chip(
            "Everyone",
            if kq.is_empty() {
                "map".to_string()
            } else {
                format!("map?{}", kq.trim_start_matches("&amp;"))
            },
            who.is_none(),
        );
        for l in &viewer.labels {
            libs += &chip(
                l,
                format!("map?who={}{kq}", esc(l)),
                who == Some(l.as_str()),
            );
        }
        libs += "</nav>";
    }
    let base = match who {
        Some(w) => format!("map?who={}", esc(w)),
        None => "map".to_string(),
    };
    format!(
        "<div class=\"mapbar\"><h1>Map</h1>{libs}{}</div>\n",
        crate::web::kind_switch(&base, kind)
    )
}

#[derive(Deserialize)]
pub struct ClusterQuery {
    z: Option<u8>,
    bbox: Option<String>,
    who: Option<String>,
    kind: Option<String>,
}

/// GET /map/clusters?z=&bbox=w,s,e,n[&who=]: the viewer's clusters as JSON.
/// It works without a map file, so the clusters stay testable and usable.
pub async fn clusters_json(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    Query(q): Query<ClusterQuery>,
) -> Response {
    let Some(labels) = map_scope(&viewer, q.who.as_deref()) else {
        return (StatusCode::NOT_FOUND, "no such library").into_response();
    };
    let bbox = match q.bbox.as_deref() {
        None => None,
        Some(b) => match parse_bbox(b) {
            Some(b) => Some(b),
            None => {
                return (StatusCode::BAD_REQUEST, "bbox: expected w,s,e,n").into_response();
            }
        },
    };
    let z = q.z.unwrap_or(0);
    let idx = st.index.clone();
    let map = st.map.clone();
    let dups = st.dups.clone();
    let me = crate::web::person(&viewer);
    let (kind, _) = crate::web::kind_filter(q.kind.as_deref());
    let res = tokio::task::spawn_blocking(move || {
        let points = map.points(&idx)?;
        // One copy of each duplicate group, and nothing this person hid,
        // as on the timeline.
        let hidden = idx.hidden(&me)?;
        let shown: Vec<Point> = points
            .iter()
            .filter(|p| kind.is_none_or(|v| p.video == v))
            .filter(|p| dups.shown(p.id, &labels, &hidden).shown)
            .cloned()
            .collect();
        anyhow::Ok(clusters(&shown, &labels, z, bbox))
    })
    .await;
    match res {
        Ok(Ok(c)) => (
            [(header::CACHE_CONTROL, "private, no-store")],
            axum::Json(c),
        )
            .into_response(),
        Ok(Err(e)) => crate::web::internal(e),
        Err(e) => crate::web::internal(e),
    }
}

/// GET /map/tiles.pmtiles: the configured PMTiles file, with byte ranges.
pub async fn tiles(State(st): State<AppState>, headers: axum::http::HeaderMap) -> Response {
    let Some(path) = st.map.pmtiles.clone() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let res = tokio::task::spawn_blocking(move || -> std::io::Result<_> {
        let f = std::fs::File::open(&path)?;
        let m = f.metadata()?;
        let mtime = m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        Ok((f, m.len(), mtime))
    })
    .await;
    let (f, len, mtime) = match res {
        Ok(Ok(x)) => x,
        Ok(Err(e)) => return crate::web::internal(e),
        Err(e) => return crate::web::internal(e),
    };
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    crate::web::ranged(
        f,
        len,
        range,
        "application/vnd.pmtiles",
        "private, max-age=86400",
        Some(format!("\"{len:x}-{mtime:x}\"")),
    )
}

/// The served path of a glyph or sprite file, checked before any file
/// system access: only plain components under `fonts/` or `sprites/`, and
/// only the three file types MapLibre asks for.
pub fn asset_path(path: &str) -> Option<(PathBuf, &'static str)> {
    let ct = match path.rsplit_once('.')?.1 {
        "pbf" => "application/x-protobuf",
        "json" => "application/json",
        "png" => "image/png",
        _ => return None,
    };
    let rel = Path::new(path);
    if path.len() > 512 || !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return None;
    }
    match rel.components().next() {
        Some(Component::Normal(top)) if top == "fonts" || top == "sprites" => {}
        _ => return None,
    }
    Some((rel.to_path_buf(), ct))
}

/// Glyph PBFs are about 100 kB, sprite sheets less than a few MB.
const ASSET_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// GET /map/assets/{*path}: a glyph or sprite file from the configured
/// directory, confined to it like a source root (SSC-004).
pub async fn assets(State(st): State<AppState>, UrlPath(path): UrlPath<String>) -> Response {
    let (Some(dir), Some((rel, ct))) = (st.map.assets.clone(), asset_path(&path)) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let res = tokio::task::spawn_blocking(move || {
        crate::source::read_limited(&dir, &rel, ASSET_MAX_BYTES)
    })
    .await;
    match res {
        Ok(Ok(b)) => (
            [
                (header::CONTENT_TYPE, ct),
                (header::CACHE_CONTROL, "private, max-age=86400"),
            ],
            b,
        )
            .into_response(),
        Ok(Err(_)) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => crate::web::internal(e),
    }
}
