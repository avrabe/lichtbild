//! Offline place names: the nearest town to a photo's position, from a
//! GeoNames file the deployment supplies (REQ-PLACE-001, DD-011). Nothing
//! is looked up outside the house (SSC-003).
//!
//! Inputs, all tab-separated GeoNames dumps:
//! - cities (e.g. `cities1000.txt`): id, name, asciiname, alternatenames,
//!   latitude, longitude, feature class, feature code, country code, cc2,
//!   admin1 code, ...
//! - admin1 names (optional, `admin1CodesASCII.txt`): "CC.code", name, ...
//! - country names (optional, `countryInfo.txt`): ISO, ISO3, numeric, fips,
//!   country name, ... (lines starting with '#' are comments)

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct Place {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub country: String,
    pub admin1: String,
}

#[derive(Debug, Default)]
pub struct Places {
    places: Vec<Place>,
    /// One-degree cells: (floor(lat), floor(lon)) -> indexes into `places`.
    grid: HashMap<(i32, i32), Vec<u32>>,
    admin1: HashMap<String, String>,
    countries: HashMap<String, String>,
    /// Lines of the cities file that could not be read.
    pub skipped_lines: usize,
    /// Answers already worked out, by exact position: photos keep their
    /// positions, so the map, Countries and Trips pages ask the same
    /// questions on every request (a dense cell holds hundreds of towns).
    memo: std::sync::Mutex<HashMap<(u64, u64), Found>>,
}

/// A search's answer: the town's index and its distance in km.
type Found = Option<(u32, f64)>;

/// At most this many positions are remembered; then the memo starts over.
const MEMO_MAX: usize = 500_000;

/// A lookup result: the place, how far away it is, and a display label.
#[derive(Debug, Clone, PartialEq)]
pub struct Nearest {
    pub label: String,
    pub km: f64,
}

fn cell(lat: f64, lon: f64) -> (i32, i32) {
    (lat.floor() as i32, lon.floor() as i32)
}

/// Great-circle distance in kilometres.
pub fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = (lat2 - lat1).to_radians();
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    6371.0 * 2.0 * a.sqrt().atan2((1.0 - a).sqrt())
}

impl Places {
    /// Parse the GeoNames files from text.
    pub fn parse(cities: &str, admin1: Option<&str>, countries: Option<&str>) -> Places {
        let mut p = Places::default();
        for line in cities.lines().filter(|l| !l.trim().is_empty()) {
            let f: Vec<&str> = line.split('\t').collect();
            let parsed = (|| {
                let lat: f64 = f.get(4)?.parse().ok()?;
                let lon: f64 = f.get(5)?.parse().ok()?;
                if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                    return None;
                }
                Some(Place {
                    name: f.get(1)?.to_string(),
                    lat,
                    lon,
                    country: f.get(8)?.to_string(),
                    admin1: f.get(10).unwrap_or(&"").to_string(),
                })
            })();
            match parsed {
                Some(place) if !place.name.is_empty() => {
                    let i = p.places.len() as u32;
                    p.grid
                        .entry(cell(place.lat, place.lon))
                        .or_default()
                        .push(i);
                    p.places.push(place);
                }
                _ => p.skipped_lines += 1,
            }
        }
        for line in admin1.unwrap_or("").lines() {
            let f: Vec<&str> = line.split('\t').collect();
            if let (Some(k), Some(v)) = (f.first(), f.get(1)) {
                p.admin1.insert(k.to_string(), v.to_string());
            }
        }
        for line in countries
            .unwrap_or("")
            .lines()
            .filter(|l| !l.starts_with('#'))
        {
            let f: Vec<&str> = line.split('\t').collect();
            if let (Some(k), Some(v)) = (f.first(), f.get(4)) {
                p.countries.insert(k.to_string(), v.to_string());
            }
        }
        p
    }

    /// Read the files the deployment names; only the cities file is required.
    pub fn load(cities: &Path, admin1: Option<&Path>, countries: Option<&Path>) -> Result<Places> {
        let read = |p: &Path| {
            std::fs::read_to_string(p).with_context(|| format!("places file {}", p.display()))
        };
        let c = read(cities)?;
        let a = admin1.map(read).transpose()?;
        let n = countries.map(read).transpose()?;
        Ok(Places::parse(&c, a.as_deref(), n.as_deref()))
    }

    pub fn len(&self) -> usize {
        self.places.len()
    }

    pub fn is_empty(&self) -> bool {
        self.places.is_empty()
    }

    /// The nearest town within about two degrees, labelled
    /// "Name, Region, Country" with whatever names are known.
    pub fn nearest(&self, lat: f64, lon: f64) -> Option<Nearest> {
        let (i, km) = self.nearest_index(lat, lon)?;
        let pl = &self.places[i as usize];
        let mut parts = vec![pl.name.clone()];
        if let Some(a) = self.admin1.get(&format!("{}.{}", pl.country, pl.admin1)) {
            if a != &pl.name {
                parts.push(a.clone());
            }
        }
        parts.push(self.country_name(&pl.country));
        Some(Nearest {
            label: parts.join(", "),
            km,
        })
    }

    /// The country of the nearest town (REQ-MAP-003): its ISO code.
    pub fn country(&self, lat: f64, lon: f64) -> Option<&str> {
        let (i, _) = self.nearest_index(lat, lon)?;
        Some(self.places[i as usize].country.as_str())
    }

    /// A country's name from countryInfo.txt, else its code.
    pub fn country_name(&self, code: &str) -> String {
        self.countries
            .get(code)
            .cloned()
            .unwrap_or_else(|| code.to_string())
    }

    fn nearest_index(&self, lat: f64, lon: f64) -> Option<(u32, f64)> {
        let key = (lat.to_bits(), lon.to_bits());
        if let Some(hit) = self
            .memo
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            return *hit;
        }
        let found = self.search(lat, lon);
        let mut m = self.memo.lock().unwrap_or_else(|e| e.into_inner());
        if m.len() >= MEMO_MAX {
            m.clear();
        }
        m.insert(key, found);
        found
    }

    fn search(&self, lat: f64, lon: f64) -> Option<(u32, f64)> {
        let (cy, cx) = cell(lat, lon);
        let mut best: Option<(u32, f64)> = None;
        // Ring 1 (3x3 cells) holds the answer unless the area is empty;
        // ring 2 covers sparse regions.
        for ring in 1..=2 {
            for dy in -ring..=ring {
                for dx in -ring..=ring {
                    let Some(ids) = self.grid.get(&(cy + dy, cx + dx)) else {
                        continue;
                    };
                    for &i in ids {
                        let pl = &self.places[i as usize];
                        let d = haversine_km(lat, lon, pl.lat, pl.lon);
                        if best.is_none_or(|(_, bd)| d < bd) {
                            best = Some((i, d));
                        }
                    }
                }
            }
            // A hit closer than one cell's width cannot be beaten further out.
            if best.is_some_and(|(_, d)| d < 100.0) {
                break;
            }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CITIES: &str = "1\tOstdorf\tOstdorf\t\t10.52\t20.27\tP\tPPL\tZZ\t\t01\t\t\t\t1200\n\
2\tWestdorf\tWestdorf\t\t10.40\t19.90\tP\tPPL\tZZ\t\t01\t\t\t\t3000\n\
3\tSuedstadt\tSuedstadt\t\t-33.90\t151.20\tP\tPPLA\tYY\t\t02\t\t\t\t90000\n\
broken line\n";
    const ADMIN1: &str = "ZZ.01\tNordprovinz\tNordprovinz\t1\nYY.02\tKuestenland\tKuestenland\t2\n";
    const COUNTRIES: &str = "#ISO\tISO3\tnum\tfips\tCountry\nZZ\tZZZ\t999\tZZ\tTestland\n";

    #[test]
    fn nearest_town_with_region_and_country() {
        let p = Places::parse(CITIES, Some(ADMIN1), Some(COUNTRIES));
        assert_eq!((p.len(), p.skipped_lines), (3, 1));
        let n = p.nearest(10.5, 20.25).unwrap();
        assert_eq!(n.label, "Ostdorf, Nordprovinz, Testland");
        assert!(n.km < 4.0, "{}", n.km);
        // Unknown country name: the code is shown.
        let s = p.nearest(-33.85, 151.21).unwrap();
        assert_eq!(s.label, "Suedstadt, Kuestenland, YY");
        // Nothing within two degrees.
        assert_eq!(p.nearest(60.0, -30.0), None);
    }

    #[test]
    fn nearest_crosses_cell_borders() {
        // 10.999 is in cell 10, Ostdorf-like town just over the border in 11.
        let p = Places::parse(
            "1\tNear\tNear\t\t11.001\t20.0\tP\tPPL\tZZ\t\t01\n2\tFar\tFar\t\t10.2\t20.0\tP\tPPL\tZZ\t\t01\n",
            None,
            None,
        );
        assert_eq!(p.nearest(10.999, 20.0).unwrap().label, "Near, ZZ");
    }

    #[test]
    fn a_remembered_answer_is_the_same_answer() {
        let p = Places::parse(CITIES, Some(ADMIN1), Some(COUNTRIES));
        for _ in 0..2 {
            assert_eq!(p.country(10.5, 20.25), Some("ZZ"));
            assert_eq!(p.country(-33.85, 151.21), Some("YY"));
            assert_eq!(p.country(60.0, -30.0), None);
        }
        assert_eq!(
            p.memo.lock().unwrap().len(),
            3,
            "each position searched once"
        );
        assert_eq!(
            p.nearest(10.5, 20.25).unwrap().label,
            "Ostdorf, Nordprovinz, Testland"
        );
    }

    // GeoNames ships countryInfo.txt with CRLF line endings (smithy,
    // 2026-11-14 snapshot); no '\r' may reach a name or skip a line.
    #[test]
    fn crlf_line_endings() {
        // Lines that end right after the column lichtbild reads, so a
        // leftover '\r' would land in the admin1 code or a name.
        let cities = "1\tOstdorf\tOstdorf\t\t10.52\t20.27\tP\tPPL\tZZ\t\t01\r\n\
2\tWestdorf\tWestdorf\t\t10.40\t19.90\tP\tPPL\tZZ\t\t01\r\n";
        let admin1 = "ZZ.01\tNordprovinz\r\n";
        let countries = "#ISO\tISO3\tISO-Numeric\tfips\tCountry\r\nZZ\tZZZ\t999\tZZ\tTestland\r\n";
        let p = Places::parse(cities, Some(admin1), Some(countries));
        assert_eq!((p.len(), p.skipped_lines), (2, 0));
        let n = p.nearest(10.5, 20.25).unwrap();
        assert_eq!(n.label, "Ostdorf, Nordprovinz, Testland");
        assert!(!n.label.contains('\r'));
    }

    #[test]
    fn distance() {
        let d = haversine_km(0.0, 0.0, 0.0, 1.0);
        assert!((d - 111.19).abs() < 0.1, "{d}");
    }
}
