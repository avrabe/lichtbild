//! Configuration from LICHTBILD_* variables and an optional TOML file
//! (REQ-CFG-001, DD-009), and the overlap check between the data directory
//! and the source roots (REQ-CFG-002, SC-001).

use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// One person's library folder, read-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    /// Short label used in URLs and the report (letters, digits, '-', '_').
    pub label: String,
    /// Canonical absolute path.
    pub path: PathBuf,
}

/// Limits that bound decoder and cache resource use (SC-005).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    pub max_pixels: u64,
    pub max_file_bytes: u64,
    pub thumb_cache_bytes: u64,
    pub thumb_size: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_pixels: 100_000_000,
            max_file_bytes: 200 * 1024 * 1024,
            thumb_cache_bytes: 20 * 1024 * 1024 * 1024,
            thumb_size: 360,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub roots: Vec<Root>,
    /// Canonical absolute path of lichtbild's own data directory.
    pub data_dir: PathBuf,
    /// "" or a prefix like "/lichtbild".
    pub base_path: String,
    pub listen: SocketAddr,
    pub workers: usize,
    /// Hours between scans inside `serve`; 0 scans only at start.
    pub scan_interval_hours: u64,
    pub limits: Limits,
    /// Opt-in to listen on a non-loopback address before login exists
    /// (REQ-AUTH-001).
    pub allow_remote_without_login: bool,
    /// Host names requests may carry besides localhost (REQ-WEB-005).
    pub allowed_hosts: Vec<String>,
    /// GeoNames files for offline place names (REQ-PLACE-001).
    pub places: PlacesFiles,
    /// Login through the house OIDC provider (REQ-AUTH-001); None keeps
    /// operator mode (loopback only, no login).
    pub oidc: Option<crate::auth::OidcSettings>,
    pub people: Vec<crate::auth::Person>,
    /// The nav v1 switcher's HTML (REQ-NAV-001); empty when not configured.
    pub nav_html: String,
    /// Transcripts of videos (REQ-MEDIA-004); None when no model is
    /// configured.
    pub transcribe: Option<Transcribe>,
    /// The OCR model directory, checked (REQ-TEXT-001); None: no reading.
    pub ocr_dir: Option<PathBuf>,
    /// The world map's PMTiles file and glyph/sprite directory
    /// (REQ-MAP-001); canonical paths, each optional.
    pub map: MapFiles,
}

/// The OIDC block of the TOML file.
#[derive(Debug, Default, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OidcFile {
    pub issuer: Option<String>,
    pub client_id: Option<String>,
    pub client_secret_file: Option<PathBuf>,
    pub ca_file: Option<PathBuf>,
    #[serde(default)]
    pub trusted_audiences: Vec<String>,
}

/// Paths of the GeoNames dumps; only `cities` is required to enable names.
#[derive(Debug, Default, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PlacesFiles {
    pub cities: Option<PathBuf>,
    pub admin1: Option<PathBuf>,
    pub countries: Option<PathBuf>,
}

/// The `[transcribe]` block: the Whisper model directory and the language
/// codes detection may choose from (all when empty).
#[derive(Debug, Default, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TranscribeFiles {
    pub model_dir: Option<PathBuf>,
    #[serde(default)]
    pub languages: Vec<String>,
}

/// A checked transcription setup.
#[derive(Debug, Clone)]
pub struct Transcribe {
    pub model_dir: PathBuf,
    /// Name of the pinned model (crate::stt::MODELS).
    pub model: &'static str,
    pub languages: Vec<String>,
}

/// The world map's files (REQ-MAP-001): one Protomaps PMTiles file, and a
/// directory with `fonts/` (glyph PBFs) and `sprites/v4/`, as in
/// protomaps/basemaps-assets. Both are read-only and optional.
#[derive(Debug, Default, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MapFiles {
    pub pmtiles: Option<PathBuf>,
    pub assets: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRoot {
    label: String,
    path: PathBuf,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileLimits {
    max_pixels: Option<u64>,
    max_file_mib: Option<u64>,
    thumb_cache_mib: Option<u64>,
    thumb_size: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    data_dir: Option<PathBuf>,
    base_path: Option<String>,
    listen: Option<String>,
    workers: Option<usize>,
    scan_interval_hours: Option<u64>,
    #[serde(default)]
    allowed_hosts: Vec<String>,
    #[serde(default)]
    places: PlacesFiles,
    #[serde(default)]
    transcribe: TranscribeFiles,
    ocr_dir: Option<PathBuf>,
    #[serde(default)]
    map: MapFiles,
    public_url: Option<String>,
    #[serde(default)]
    oidc: OidcFile,
    #[serde(default)]
    people: Vec<crate::auth::Person>,
    #[serde(default)]
    roots: Vec<FileRoot>,
    #[serde(default)]
    limits: FileLimits,
}

/// Raw settings before validation; the environment overrides the file.
#[derive(Debug, Default, Clone)]
pub struct Settings {
    pub roots: Vec<(String, PathBuf)>,
    pub data_dir: Option<PathBuf>,
    pub base_path: Option<String>,
    pub listen: Option<String>,
    pub workers: Option<usize>,
    pub scan_interval_hours: Option<u64>,
    pub limits: Limits,
    pub allow_remote_without_login: bool,
    pub allowed_hosts: Vec<String>,
    pub places: PlacesFiles,
    pub transcribe: TranscribeFiles,
    pub ocr_dir: Option<PathBuf>,
    pub map: MapFiles,
    pub public_url: Option<String>,
    pub oidc: OidcFile,
    pub people: Vec<crate::auth::Person>,
    /// LICHTBILD_NAV (the nav v1 file) and LICHTBILD_NAV_SELF (this app's id).
    pub nav_file: Option<PathBuf>,
    pub nav_self: Option<String>,
}

impl Settings {
    /// Read LICHTBILD_CONFIG (if set) and then the LICHTBILD_* variables.
    pub fn from_env() -> Result<Settings> {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let mut s = Settings::default();
        if let Some(file) = get("LICHTBILD_CONFIG") {
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("LICHTBILD_CONFIG {file}"))?;
            s.apply_file(&text)
                .with_context(|| format!("LICHTBILD_CONFIG {file}"))?;
        }
        if let Some(v) = get("LICHTBILD_ROOTS") {
            s.roots = parse_roots(&v)?;
        }
        if let Some(v) = get("LICHTBILD_DATA_DIR") {
            s.data_dir = Some(PathBuf::from(v));
        }
        if let Some(v) = get("LICHTBILD_BASE_PATH") {
            s.base_path = Some(v);
        }
        if let Some(v) = get("LICHTBILD_LISTEN") {
            s.listen = Some(v);
        }
        if let Some(v) = get("LICHTBILD_WORKERS") {
            s.workers = Some(v.parse().context("LICHTBILD_WORKERS")?);
        }
        if let Some(v) = get("LICHTBILD_SCAN_INTERVAL_HOURS") {
            s.scan_interval_hours = Some(v.parse().context("LICHTBILD_SCAN_INTERVAL_HOURS")?);
        }
        let num = |k: &str| -> Result<Option<u64>> {
            get(k)
                .map(|v| v.parse::<u64>().with_context(|| k.to_string()))
                .transpose()
        };
        if let Some(v) = num("LICHTBILD_MAX_PIXELS")? {
            s.limits.max_pixels = v;
        }
        if let Some(v) = num("LICHTBILD_MAX_FILE_MIB")? {
            s.limits.max_file_bytes = v * 1024 * 1024;
        }
        if let Some(v) = num("LICHTBILD_THUMB_CACHE_MIB")? {
            s.limits.thumb_cache_bytes = v * 1024 * 1024;
        }
        if let Some(v) = num("LICHTBILD_THUMB_SIZE")? {
            s.limits.thumb_size = u32::try_from(v).context("LICHTBILD_THUMB_SIZE")?;
        }
        if let Some(v) = get("LICHTBILD_NAV") {
            s.nav_file = Some(PathBuf::from(v));
        }
        if let Some(v) = get("LICHTBILD_NAV_SELF") {
            s.nav_self = Some(v);
        }
        if let Some(v) = get("LICHTBILD_PUBLIC_URL") {
            s.public_url = Some(v);
        }
        if let Some(v) = get("LICHTBILD_OIDC_ISSUER") {
            s.oidc.issuer = Some(v);
        }
        if let Some(v) = get("LICHTBILD_OIDC_CLIENT_ID") {
            s.oidc.client_id = Some(v);
        }
        if let Some(v) = get("LICHTBILD_OIDC_CLIENT_SECRET_FILE") {
            s.oidc.client_secret_file = Some(PathBuf::from(v));
        }
        if let Some(v) = get("LICHTBILD_OIDC_CA_FILE") {
            s.oidc.ca_file = Some(PathBuf::from(v));
        }
        if let Some(v) = get("LICHTBILD_OIDC_TRUSTED_AUDIENCES") {
            s.oidc.trusted_audiences = v
                .split(',')
                .map(|a| a.trim().to_string())
                .filter(|a| !a.is_empty())
                .collect();
        }
        if let Some(v) = get("LICHTBILD_WHISPER_DIR") {
            s.transcribe.model_dir = Some(PathBuf::from(v));
        }
        if let Some(v) = get("LICHTBILD_TRANSCRIBE_LANGUAGES") {
            s.transcribe.languages = v
                .split(',')
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
        }
        if let Some(v) = get("LICHTBILD_OCR_DIR") {
            s.ocr_dir = Some(PathBuf::from(v));
        }
        if let Some(v) = get("LICHTBILD_PLACES") {
            s.places.cities = Some(PathBuf::from(v));
        }
        if let Some(v) = get("LICHTBILD_PLACES_ADMIN1") {
            s.places.admin1 = Some(PathBuf::from(v));
        }
        if let Some(v) = get("LICHTBILD_PLACES_COUNTRIES") {
            s.places.countries = Some(PathBuf::from(v));
        }
        if let Some(v) = get("LICHTBILD_MAP_PMTILES") {
            s.map.pmtiles = Some(PathBuf::from(v));
        }
        if let Some(v) = get("LICHTBILD_MAP_ASSETS") {
            s.map.assets = Some(PathBuf::from(v));
        }
        if let Some(v) = get("LICHTBILD_ALLOWED_HOSTS") {
            s.allowed_hosts = v
                .split(',')
                .map(|h| h.trim().to_string())
                .filter(|h| !h.is_empty())
                .collect();
        }
        if let Some(v) = get("LICHTBILD_ALLOW_REMOTE_WITHOUT_LOGIN") {
            s.allow_remote_without_login = v == "1";
        }
        Ok(s)
    }

    pub fn apply_file(&mut self, text: &str) -> Result<()> {
        let f: FileConfig = toml::from_str(text)?;
        if !f.roots.is_empty() {
            self.roots = f.roots.into_iter().map(|r| (r.label, r.path)).collect();
        }
        self.data_dir = f.data_dir.or(self.data_dir.take());
        self.base_path = f.base_path.or(self.base_path.take());
        self.listen = f.listen.or(self.listen.take());
        self.workers = f.workers.or(self.workers);
        if !f.allowed_hosts.is_empty() {
            self.allowed_hosts = f.allowed_hosts;
        }
        if f.transcribe != TranscribeFiles::default() {
            self.transcribe = f.transcribe;
        }
        self.ocr_dir = f.ocr_dir.or(self.ocr_dir.take());
        if f.places != PlacesFiles::default() {
            self.places = f.places;
        }
        self.map.pmtiles = f.map.pmtiles.or(self.map.pmtiles.take());
        self.map.assets = f.map.assets.or(self.map.assets.take());
        self.public_url = f.public_url.or(self.public_url.take());
        let o = &mut self.oidc;
        o.issuer = f.oidc.issuer.or(o.issuer.take());
        o.client_id = f.oidc.client_id.or(o.client_id.take());
        o.client_secret_file = f.oidc.client_secret_file.or(o.client_secret_file.take());
        o.ca_file = f.oidc.ca_file.or(o.ca_file.take());
        if !f.oidc.trusted_audiences.is_empty() {
            o.trusted_audiences = f.oidc.trusted_audiences;
        }
        if !f.people.is_empty() {
            self.people = f.people;
        }
        self.scan_interval_hours = f.scan_interval_hours.or(self.scan_interval_hours);
        let l = &mut self.limits;
        if let Some(v) = f.limits.max_pixels {
            l.max_pixels = v;
        }
        if let Some(v) = f.limits.max_file_mib {
            l.max_file_bytes = v * 1024 * 1024;
        }
        if let Some(v) = f.limits.thumb_cache_mib {
            l.thumb_cache_bytes = v * 1024 * 1024;
        }
        if let Some(v) = f.limits.thumb_size {
            l.thumb_size = v;
        }
        Ok(())
    }

    /// Validate and check overlaps. Nothing is created: the data directory
    /// must already exist (the deployment makes it), so a typo or a path
    /// through a symlink into a root fails before any write (SC-001).
    pub fn resolve(self) -> Result<Config> {
        if self.roots.is_empty() {
            bail!("no source roots: set LICHTBILD_ROOTS (label=/path;label=/path) or [[roots]] in LICHTBILD_CONFIG");
        }
        let data_dir = self
            .data_dir
            .context("no data directory: set LICHTBILD_DATA_DIR or data_dir in LICHTBILD_CONFIG")?;
        if !data_dir.is_absolute() {
            bail!("data directory must be an absolute path");
        }
        let mut roots = Vec::new();
        for (label, path) in self.roots {
            check_label(&label)?;
            if roots.iter().any(|r: &Root| r.label == label) {
                bail!("root label {label:?} is given twice");
            }
            if !path.is_absolute() {
                bail!("root {label:?}: path must be absolute");
            }
            let canon = path
                .canonicalize()
                .with_context(|| format!("root {label:?}: cannot open its folder"))?;
            if !canon.is_dir() {
                bail!("root {label:?}: not a directory");
            }
            roots.push(Root { label, path: canon });
        }
        // Check the lexical path and the resolved path; create nothing.
        check_overlap(&roots, &normalize(&data_dir))?;
        let data_dir = data_dir
            .canonicalize()
            .context("data directory does not exist or cannot be opened; create it first")?;
        if !data_dir.is_dir() {
            bail!("data directory is not a directory");
        }
        check_overlap(&roots, &data_dir)?;

        let base_path = parse_base_path(self.base_path.as_deref())?;
        let oidc = match &self.oidc.issuer {
            None => {
                if !self.people.is_empty() {
                    bail!("[[people]] is set but LICHTBILD_OIDC_ISSUER is not");
                }
                None
            }
            Some(issuer) => {
                let public_url = self
                    .public_url
                    .clone()
                    .context("LICHTBILD_OIDC_ISSUER needs LICHTBILD_PUBLIC_URL")?;
                let public_url = public_url.trim_end_matches('/').to_string();
                let path_ok = public_url
                    .split_once("://")
                    .map(|(_, rest)| rest.find('/').map_or("", |i| &rest[i..]))
                    .is_some_and(|p| p == base_path);
                if !path_ok {
                    bail!("LICHTBILD_PUBLIC_URL must end with the base path {base_path:?}");
                }
                if self.people.is_empty() {
                    bail!("login is configured but no [[people]] are");
                }
                Some(crate::auth::OidcSettings {
                    issuer: issuer.clone(),
                    client_id: self
                        .oidc
                        .client_id
                        .clone()
                        .context("LICHTBILD_OIDC_CLIENT_ID")?,
                    client_secret_file: self
                        .oidc
                        .client_secret_file
                        .clone()
                        .context("LICHTBILD_OIDC_CLIENT_SECRET_FILE")?,
                    ca_file: self.oidc.ca_file.clone(),
                    trusted_audiences: self.oidc.trusted_audiences.clone(),
                    public_url,
                })
            }
        };
        let listen: SocketAddr = self
            .listen
            .as_deref()
            .unwrap_or("127.0.0.1:8080")
            .parse()
            .context("listen address")?;
        let workers = self.workers.unwrap_or(2).clamp(1, 16);
        Ok(Config {
            roots,
            data_dir,
            base_path,
            listen,
            workers,
            scan_interval_hours: self.scan_interval_hours.unwrap_or(24),
            limits: self.limits,
            allow_remote_without_login: self.allow_remote_without_login,
            // Same normalisation as the request side: no port, no trailing dot.
            allowed_hosts: self
                .allowed_hosts
                .iter()
                .map(|h| crate::web::host_name(h))
                .collect(),
            places: self.places,
            transcribe: match self.transcribe.model_dir {
                // The model is checked against its pinned hash at startup,
                // so a wrong or truncated file stops the server, not the
                // first transcription hours later.
                Some(dir) => Some(Transcribe {
                    model: crate::stt::check_model(&dir)?,
                    model_dir: dir,
                    languages: self.transcribe.languages,
                }),
                None => None,
            },
            // Checked at startup too: a wrong file stops the server.
            ocr_dir: match self.ocr_dir {
                Some(dir) => {
                    crate::ocr::check_model(&dir)?;
                    Some(dir)
                }
                None => None,
            },
            oidc,
            people: self.people,
            nav_html: nav_html(self.nav_file.as_deref(), self.nav_self.as_deref())?,
            map: crate::map::check_files(&self.map)?,
        })
    }
}

/// The nav switcher, as haus does it: both settings or neither; a file that
/// breaks the contract stops startup with the field named.
pub fn nav_html(file: Option<&Path>, me: Option<&str>) -> Result<String> {
    match (file, me.filter(|s| !s.is_empty())) {
        (None, None) => Ok(String::new()),
        (Some(_), None) => bail!("LICHTBILD_NAV is set but LICHTBILD_NAV_SELF is not"),
        (None, Some(_)) => bail!("LICHTBILD_NAV_SELF is set but LICHTBILD_NAV is not"),
        (Some(f), Some(me)) => {
            let json = std::fs::read_to_string(f)
                .with_context(|| format!("LICHTBILD_NAV {}", f.display()))?;
            let nav =
                crate::nav::parse(&json, me).map_err(|e| anyhow::anyhow!("LICHTBILD_NAV: {e}"))?;
            Ok(crate::nav::render(&nav, me, crate::web::esc))
        }
    }
}

/// `label=/path;label=/path`
pub fn parse_roots(v: &str) -> Result<Vec<(String, PathBuf)>> {
    v.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|pair| {
            let (l, p) = pair
                .split_once('=')
                .context("LICHTBILD_ROOTS: expected label=/path pairs separated by ';'")?;
            Ok((l.trim().to_string(), PathBuf::from(p.trim())))
        })
        .collect()
}

fn check_label(label: &str) -> Result<()> {
    let ok = !label.is_empty()
        && label.len() <= 32
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && label != "all";
    if !ok {
        bail!("root label {label:?}: use 1-32 letters, digits, '-' or '_' (and not \"all\")");
    }
    Ok(())
}

/// SC-001: no root inside the data directory, no data directory inside a
/// root, no root inside another root.
pub fn check_overlap(roots: &[Root], data_dir: &Path) -> Result<()> {
    for (i, r) in roots.iter().enumerate() {
        if data_dir.starts_with(&r.path) || r.path.starts_with(data_dir) {
            bail!(
                "data directory and root {:?} overlap; lichtbild never writes inside a root",
                r.label
            );
        }
        for o in &roots[i + 1..] {
            if o.path.starts_with(&r.path) || r.path.starts_with(&o.path) {
                bail!("roots {:?} and {:?} overlap", r.label, o.label);
            }
        }
    }
    Ok(())
}

/// Lexical normalisation: drop "." and resolve ".." without touching the disk.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// LICHTBILD_BASE_PATH: unset, empty or "/" is no prefix; otherwise "/"
/// followed by segments of letters, digits, '-', '_' and '.', without "."
/// or "..". A trailing "/" is dropped. Same rule as haus's HAUS_BASE_PATH.
pub fn parse_base_path(v: Option<&str>) -> Result<String> {
    let v = v.unwrap_or("").trim();
    let v = v.strip_suffix('/').unwrap_or(v);
    if v.is_empty() {
        return Ok(String::new());
    }
    let ok = v.starts_with('/')
        && v[1..].split('/').all(|seg| {
            !seg.is_empty()
                && seg != "."
                && seg != ".."
                && seg
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        });
    if !ok {
        bail!("LICHTBILD_BASE_PATH {v:?}: expected a path like /lichtbild");
    }
    Ok(v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_path_rules() {
        assert_eq!(parse_base_path(None).unwrap(), "");
        assert_eq!(parse_base_path(Some("/")).unwrap(), "");
        assert_eq!(parse_base_path(Some("/photos/")).unwrap(), "/photos");
        assert!(parse_base_path(Some("photos")).is_err());
        assert!(parse_base_path(Some("/a/../b")).is_err());
        assert!(parse_base_path(Some("/a b")).is_err());
    }

    #[test]
    fn roots_from_env_string() {
        let r = parse_roots("a=/x/y; b = /z ;").unwrap();
        assert_eq!(
            r,
            vec![
                ("a".to_string(), PathBuf::from("/x/y")),
                ("b".to_string(), PathBuf::from("/z"))
            ]
        );
        assert!(parse_roots("nolabel").is_err());
    }

    #[test]
    fn places_from_file() {
        let mut s = Settings::default();
        s.apply_file(
            "[places]\ncities = \"/x/cities1000.txt\"\ncountries = \"/x/countryInfo.txt\"\n",
        )
        .unwrap();
        assert_eq!(s.places.cities, Some(PathBuf::from("/x/cities1000.txt")));
        assert_eq!(s.places.admin1, None);
        assert_eq!(
            s.places.countries,
            Some(PathBuf::from("/x/countryInfo.txt"))
        );
        assert!(
            s.apply_file("[places]\ntowns = \"/x\"\n").is_err(),
            "unknown key accepted"
        );
    }

    #[test]
    fn map_files_from_file() {
        let mut s = Settings::default();
        s.apply_file("[map]\npmtiles = \"/x/world.pmtiles\"\nassets = \"/x/assets\"\n")
            .unwrap();
        assert_eq!(s.map.pmtiles, Some(PathBuf::from("/x/world.pmtiles")));
        assert_eq!(s.map.assets, Some(PathBuf::from("/x/assets")));
        assert!(
            s.apply_file("[map]\ntiles = \"/x\"\n").is_err(),
            "unknown key accepted"
        );
    }

    #[test]
    fn trusted_audiences_from_file() {
        let mut s = Settings::default();
        s.apply_file("[oidc]\ntrusted_audiences = [\"project-1\"]\n")
            .unwrap();
        assert_eq!(s.oidc.trusted_audiences, vec!["project-1".to_string()]);
    }

    #[test]
    fn labels() {
        assert!(check_label("p1").is_ok());
        assert!(check_label("all").is_err());
        assert!(check_label("a/b").is_err());
        assert!(check_label("").is_err());
    }
}
