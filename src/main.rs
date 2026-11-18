//! lichtbild binary: `scan` and `serve`.

use std::process::ExitCode;

use anyhow::{bail, Result};
use std::sync::Arc;

use lichtbild::config::{Config, Settings};
use lichtbild::index::Index;
use lichtbild::{scan, web, VERSION};

const HELP: &str = "\
lichtbild: the household's photo and video library

Usage: lichtbild [--help | --version]
       lichtbild serve    serve the timeline, scanning at start and every interval
       lichtbild scan     scan once and print the report (only while no server runs)

Configuration (environment, overriding the file named by LICHTBILD_CONFIG):
  LICHTBILD_ROOTS      label=/path;label=/path   one root per person, read-only
  LICHTBILD_DATA_DIR   /path                     index, thumbnails, report
  LICHTBILD_BASE_PATH  /prefix                   when served under a path
  LICHTBILD_LISTEN     127.0.0.1:8080
  LICHTBILD_ALLOWED_HOSTS  photos.example        host names besides localhost (comma-separated)
  LICHTBILD_WORKERS    2
  LICHTBILD_SCAN_INTERVAL_HOURS  24              0 = scan only at start
  LICHTBILD_MAX_PIXELS / _MAX_FILE_MIB / _THUMB_CACHE_MIB / _THUMB_SIZE   limits
  LICHTBILD_PUBLIC_URL https://host/prefix       external URL; needed with login
  LICHTBILD_OIDC_ISSUER / _CLIENT_ID / _CLIENT_SECRET_FILE / _CA_FILE   login (OIDC)
  LICHTBILD_OIDC_TRUSTED_AUDIENCES  id,id        extra ID-token audiences (azp must be this client)
  people: [[people]] label, oidc_subject or oidc_email, sees = [..]  (TOML only)
  LICHTBILD_PLACES     /path/cities1000.txt      GeoNames towns for offline place names
  LICHTBILD_PLACES_ADMIN1 / _COUNTRIES           admin1CodesASCII.txt / countryInfo.txt
  LICHTBILD_MAP_PMTILES  /path/world.pmtiles     Protomaps PMTiles v3 file for the map
  LICHTBILD_MAP_ASSETS   /path/basemaps-assets   its fonts/ and sprites/v4/ (map labels)
  LICHTBILD_DATA_DIR must exist; lichtbild creates nothing at startup.
  LICHTBILD_ALLOW_REMOTE_WITHOUT_LOGIN  1 to listen beyond loopback before login exists

`scan` exits 2 when found != indexed + skipped, or when the index disagrees;
`serve` logs the same as an error. The report is also written to
<data dir>/scan-report.json.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["-V" | "--version"] => {
            println!("lichtbild {VERSION}");
            ExitCode::SUCCESS
        }
        ["-h" | "--help"] | [] => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        ["scan"] => report(run_scan()),
        // Internal: the scan runs this to decode one video keyframe in a
        // separate, limited process (DD-016).
        ["decode-keyframe"] => match lichtbild::video::keyframe_child() {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::from(1),
        },
        ["serve"] => report(run_serve()),
        _ => {
            eprint!("{HELP}");
            ExitCode::from(64)
        }
    }
}

fn report(r: Result<ExitCode>) -> ExitCode {
    r.unwrap_or_else(|e| {
        eprintln!("lichtbild: {e:#}");
        ExitCode::FAILURE
    })
}

fn init_log() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lichtbild=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
}

fn run_scan() -> Result<ExitCode> {
    init_log();
    let cfg = Settings::from_env()?.resolve()?;
    let idx = Index::open(&cfg.data_dir)?;
    let rep = scan::run(&cfg, &idx, &scan::Options::default())?;
    print!("{}", rep.to_text());
    Ok(if rep.reconciled {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

fn run_serve() -> Result<ExitCode> {
    init_log();
    let cfg = Settings::from_env()?.resolve()?;
    // With login configured, lichtbild may listen beyond loopback (behind
    // the reverse proxy); without it only on loopback unless overridden.
    if !cfg.listen.ip().is_loopback() && cfg.oidc.is_none() && !cfg.allow_remote_without_login {
        bail!(
            "refusing to listen on {} without login (REQ-AUTH-001); use a loopback address behind the reverse proxy",
            cfg.listen
        );
    }
    let places = match &cfg.places.cities {
        None => None,
        Some(c) => {
            let p = lichtbild::places::Places::load(
                c,
                cfg.places.admin1.as_deref(),
                cfg.places.countries.as_deref(),
            )?;
            tracing::info!(
                "place names: {} towns ({} lines skipped)",
                p.len(),
                p.skipped_lines
            );
            Some(p)
        }
    };
    let auth = match &cfg.oidc {
        None => None,
        Some(o) => {
            let a = lichtbild::auth::Auth::new(
                o.clone(),
                cfg.people.clone(),
                cfg.roots.iter().map(|r| r.label.clone()).collect(),
            )?;
            tracing::info!("login through {} for {} people", o.issuer, cfg.people.len());
            Some(a)
        }
    };
    let idx = Arc::new(Index::open(&cfg.data_dir)?);
    spawn_scanner(cfg.clone(), idx.clone());
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let app = web::router(
            web::AppState::new(&cfg, idx)
                .with_places(places)
                .with_auth(auth),
        );
        let listener = tokio::net::TcpListener::bind(cfg.listen).await?;
        tracing::info!("listening on {}{}", cfg.listen, cfg.base_path);
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await?;
        anyhow::Ok(())
    })?;
    Ok(ExitCode::SUCCESS)
}

/// Scan in one background thread: at start, then every interval. The thread
/// lowers its own priority (on Linux nice and I/O priority are per thread,
/// and the scan's workers inherit them), so serving stays responsive.
fn spawn_scanner(cfg: Config, idx: Arc<Index>) {
    std::thread::spawn(move || loop {
        match scan::run(&cfg, &idx, &scan::Options::default()) {
            Ok(rep) if rep.reconciled => tracing::info!("scan finished\n{}", rep.to_text()),
            Ok(rep) => tracing::error!("scan NOT reconciled\n{}", rep.to_text()),
            Err(e) => tracing::error!("scan failed: {e:#}"),
        }
        if cfg.scan_interval_hours == 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(
            cfg.scan_interval_hours * 3600,
        ));
    });
}
