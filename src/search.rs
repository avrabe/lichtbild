//! One search box (REQ-SEARCH-001, DD-019): what was said in videos, place
//! names, dates, file and folder names. A tantivy index in memory, rebuilt
//! from the redb index after each scan and transcription pass; nothing is
//! written to disk. Results are scoped to the libraries the viewer may see
//! (REQ-AUTH-002) inside the query, not after it.

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

use anyhow::Result;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, Query, QueryParser, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value, FAST, STORED, STRING,
};
use tantivy::tokenizer::{
    AsciiFoldingFilter, LowerCaser, RemoveLongFilter, SimpleTokenizer, TextAnalyzer,
};
use tantivy::{doc, Index as TIndex, IndexReader, TantivyDocument, Term};

use crate::index::Index;
use crate::places::Places;

const TOKENIZER: &str = "lichtbild";
/// Documents read per query before grouping by item.
const HITS: usize = 400;

/// What one hit says about an item: where it matched.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub id: u64,
    pub label: String,
    /// Transcript lines that matched: (second, text).
    pub speech: Vec<(f32, String)>,
    /// The text read in the picture matched (REQ-TEXT-001): the lines
    /// that hold a query word, or the first ones.
    pub read: Option<String>,
}

/// Marks a document of text read in a picture (`t` of transcript lines
/// is their second, of an item's own words -1).
const READ_T: f64 = -2.0;

/// Terms weighed per suggestion request; with earlier words, each is a
/// query of its own.
const CANDIDATES: usize = 40;

struct Fields {
    id: Field,
    label: Field,
    t: Field,
    text: Field,
}

struct Built {
    reader: IndexReader,
    index: TIndex,
    f: Fields,
    docs: usize,
    vocab: Vec<Known>,
}

/// The search index the server holds; empty until the first rebuild.
#[derive(Default)]
pub struct Search {
    inner: RwLock<Option<Built>>,
}

const MONTHS: [(&str, &str); 12] = [
    ("Januar", "January"),
    ("Februar", "February"),
    ("März", "March"),
    ("April", "April"),
    ("Mai", "May"),
    ("Juni", "June"),
    ("Juli", "July"),
    ("August", "August"),
    ("September", "September"),
    ("Oktober", "October"),
    ("November", "November"),
    ("Dezember", "December"),
];

fn schema() -> (Schema, Fields) {
    let mut b = Schema::builder();
    let text_opts = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(TOKENIZER)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    let f = Fields {
        id: b.add_u64_field("id", STORED | FAST),
        label: b.add_text_field("label", STRING | STORED),
        t: b.add_f64_field("t", STORED),
        text: b.add_text_field("text", text_opts | STORED),
    };
    (b.build(), f)
}

/// The place name for a position, when the deployment has place names.
fn place_name(at: Option<(f64, f64)>, places: Option<&Places>) -> Option<String> {
    let (lat, lon) = at?;
    places?.nearest(lat, lon).map(|n| n.label)
}

/// Lower case without accents, as the index folds words: "Mädchen" and
/// "madchen" are one key.
pub fn fold(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        match c {
            'ä' | 'à' | 'á' | 'â' | 'ã' | 'å' => o.push('a'),
            'ö' | 'ò' | 'ó' | 'ô' | 'õ' | 'ø' => o.push('o'),
            'ü' | 'ù' | 'ú' | 'û' => o.push('u'),
            'é' | 'è' | 'ê' | 'ë' => o.push('e'),
            'í' | 'ì' | 'î' | 'ï' => o.push('i'),
            'ç' => o.push('c'),
            'ñ' => o.push('n'),
            'ß' => o.push_str("ss"),
            c => o.push(c),
        }
    }
    o
}

/// What a suggestion names (REQ-SEARCH-002), in the order they are offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Thing,
    Place,
    Date,
    Word,
}

/// One suggestion: the whole query it completes to, the term it adds, a
/// hint (the English name, the month in English) and how many items the
/// query finds.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub kind: Kind,
    pub query: String,
    pub term: String,
    pub hint: String,
    pub items: u64,
}

/// Words that ask for the viewer's favourites (REQ-WEB-016) rather than
/// for text in the index.
const FAV_WORDS: [&str; 8] = [
    "♥",
    "♡",
    "favoriten",
    "favorit",
    "favourites",
    "favourite",
    "favorites",
    "favorite",
];

/// The query without the words that ask for favourites, and whether any
/// did: "Favoriten Katze" is ("Katze", true).
pub fn split_fav(q: &str) -> (String, bool) {
    let mut fav = false;
    let rest: Vec<&str> = q
        .split_whitespace()
        .filter(|w| {
            let f = FAV_WORDS.contains(&fold(w).as_str());
            fav |= f;
            !f
        })
        .collect();
    (rest.join(" "), fav)
}

/// The favourites word a partly typed last word completes to, if any
/// ("Fav" -> "Favoriten", "favou" -> "favourites").
pub fn fav_completion(word: &str) -> Option<&'static str> {
    let w = fold(word);
    if w.chars().count() < 3 {
        return None;
    }
    ["favoriten", "favourites", "favorites"]
        .into_iter()
        .find(|f| f.starts_with(&w) && *f != w)
        .map(|f| match f {
            "favoriten" => "Favoriten",
            "favourites" => "favourites",
            _ => "favorites",
        })
}

/// A term as plain words for a query: characters the query parser reads
/// as operators (a leading minus, colons, quotes, brackets) become spaces.
pub fn plain(term: &str) -> String {
    term.chars()
        .map(|c| {
            if matches!(
                c,
                ':' | '"'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '^'
                    | '~'
                    | '*'
                    | '+'
                    | '-'
                    | '!'
                    | '\\'
            ) {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A term that narrows results down (REQ-SEARCH-003), and how many of
/// them hold it.
#[derive(Debug, Clone, PartialEq)]
pub struct Narrow {
    pub kind: Kind,
    pub term: String,
    pub items: usize,
}

/// Results at least, before narrowing is offered.
pub const NARROW_FROM: usize = 4;

/// The terms that split the results of `q`: held by some but not all of
/// them, not already in the query; up to four things, four years and three
/// places, the most held first. `held` lists each result's terms.
pub fn narrow(q: &str, held: &[Vec<(Kind, String)>]) -> Vec<Narrow> {
    if held.len() < NARROW_FROM {
        return Vec::new();
    }
    let typed: HashSet<String> = q.split_whitespace().map(fold).collect();
    let mut count: HashMap<(Kind, String), usize> = HashMap::new();
    for terms in held {
        let mut once: HashSet<&(Kind, String)> = HashSet::new();
        for t in terms {
            if once.insert(t) {
                *count.entry(t.clone()).or_default() += 1;
            }
        }
    }
    let mut all: Vec<Narrow> = count
        .into_iter()
        .filter(|((_, term), n)| {
            *n < held.len()
                && !term
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|w| !w.is_empty())
                    .all(|w| typed.contains(&fold(w)))
        })
        .map(|((kind, term), items)| Narrow { kind, term, items })
        .collect();
    all.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then(b.items.cmp(&a.items))
            .then(a.term.cmp(&b.term))
    });
    let mut per: HashMap<Kind, usize> = HashMap::new();
    all.retain(|n| {
        let c = per.entry(n.kind).or_default();
        *c += 1;
        *c <= if n.kind == Kind::Place { 3 } else { 4 }
    });
    all
}

/// A term search knows, with how many items of each library hold it.
struct Known {
    kind: Kind,
    text: String,
    hint: String,
    keys: Vec<String>,
    by: Vec<(String, u64)>,
}

/// A term's hint, the keys it is found by, and its items per library.
type Counted = (String, Vec<String>, HashMap<String, u64>);

#[derive(Default)]
struct Vocab {
    terms: HashMap<(Kind, String), Counted>,
}

impl Vocab {
    /// Count `text` for one item of `label`; it is found by its words, the
    /// hint's and `more`'s.
    fn add(&mut self, kind: Kind, text: &str, hint: &str, more: &str, label: &str) {
        let e = self
            .terms
            .entry((kind, text.to_string()))
            .or_insert_with(|| {
                let keys = format!("{text} {hint} {more}")
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|k| !k.is_empty())
                    .map(fold)
                    .collect();
                (hint.to_string(), keys, HashMap::new())
            });
        *e.2.entry(label.to_string()).or_default() += 1;
    }

    fn done(self) -> Vec<Known> {
        self.terms
            .into_iter()
            .map(|((kind, text), (hint, keys, by))| Known {
                kind,
                text,
                hint,
                keys,
                by: by.into_iter().collect(),
            })
            .collect()
    }
}

/// The season (meteorological) of month `m`, German and English.
fn season(m: u32) -> Option<(&'static str, &'static str)> {
    match m {
        3..=5 => Some(("Frühling", "spring")),
        6..=8 => Some(("Sommer", "summer")),
        9..=11 => Some(("Herbst", "autumn fall")),
        12 | 1 | 2 => Some(("Winter", "winter")),
        _ => None,
    }
}

/// Easter Sunday of year `y` (Gregorian, the anonymous algorithm).
fn easter(y: i32) -> Option<chrono::NaiveDate> {
    let a = y % 19;
    let b = y / 100;
    let c = y % 100;
    let d = b / 4;
    let e = b % 4;
    let f = (b + 8) / 25;
    let g = (b - f + 1) / 3;
    let h = (19 * a + b - d - g + 15) % 30;
    let i = c / 4;
    let k = c % 4;
    let l = (32 + 2 * e + 2 * i - h - k) % 7;
    let m = (a + 11 * h + 22 * l) / 451;
    let month = (h + l - 7 * m + 114) / 31;
    let day = (h + l - 7 * m + 114) % 31 + 1;
    chrono::NaiveDate::from_ymd_opt(y, month as u32, day as u32)
}

/// The holiday a day is part of, German and English: Christmas (24 to 26
/// December), New Year's Eve and Day, Easter (Good Friday to Easter
/// Monday).
fn holiday(day: chrono::NaiveDate) -> Option<(&'static str, &'static str)> {
    use chrono::Datelike;
    match (day.month(), day.day()) {
        (12, 24..=26) => return Some(("Weihnachten", "Christmas")),
        (12, 31) => return Some(("Silvester", "New Year's Eve")),
        (1, 1) => return Some(("Neujahr", "New Year")),
        _ => {}
    }
    let e = easter(day.year())?;
    let off = (day - e).num_days();
    (-2..=1).contains(&off).then_some(("Ostern", "Easter"))
}

/// The words of a date ("2021-07-14T10:00:00"): year, month, day, season
/// and holiday, in German and English.
pub fn date_words(taken: &str) -> Vec<String> {
    let mut words = Vec::new();
    let (Some(y), Some(m)) = (
        taken
            .get(..4)
            .filter(|y| y.bytes().all(|b| b.is_ascii_digit())),
        taken.get(5..7).and_then(|m| m.parse::<u32>().ok()),
    ) else {
        return words;
    };
    words.push(y.to_string());
    if let Some((de, en)) = MONTHS.get((m as usize).wrapping_sub(1)) {
        words.push(format!("{de} {en}"));
    }
    if let Some((de, en)) = season(m) {
        words.push(format!("{de} {en}"));
    }
    if let Some(day) = taken.get(..10) {
        words.push(day.to_string());
        if let Some((de, en)) = chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
            .ok()
            .and_then(holiday)
        {
            words.push(format!("{de} {en}"));
        }
    }
    words
}

/// Words describing an item without its content: names, place, date.
/// `at` is the item's position: recorded, set by hand or estimated.
pub fn item_words(
    rec: &crate::index::Record,
    at: Option<(f64, f64)>,
    places: Option<&Places>,
) -> String {
    let rel = String::from_utf8_lossy(&rec.rel);
    let mut words: Vec<String> = rel
        .split(['/', '_', '-', '.', ' '])
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect();
    if let Some(name) = place_name(at, places) {
        words.push(name);
    }
    words.extend(date_words(&rec.taken_local));
    if rec.video.is_some() {
        words.push("video".into());
    }
    words.join(" ")
}

impl Search {
    /// Rebuild from the index: one document per live item, one per
    /// transcript line. Returns the number of documents.
    pub fn rebuild(&self, idx: &Index, places: Option<&Places>) -> Result<usize> {
        let (schema, f) = schema();
        let index = TIndex::create_in_ram(schema);
        index.tokenizers().register(
            TOKENIZER,
            TextAnalyzer::builder(SimpleTokenizer::default())
                .filter(RemoveLongFilter::limit(40))
                .filter(LowerCaser)
                .filter(AsciiFoldingFilter)
                .build(),
        );
        let mut w = index.writer_with_num_threads(1, 50_000_000)?;
        let mut docs = 0usize;
        let items = idx.all()?;
        let pos = crate::place::positions(&items, &idx.placed()?);
        let mut vocab = Vocab::default();
        // The same content twice in a library counts once.
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for (id, rec) in items {
            if rec.missing {
                continue;
            }
            let at = pos.get(&id).map(|p| (p.lat, p.lon));
            let objects = idx.objects(&rec.hash)?;
            let count = seen.insert((rec.label.clone(), rec.hash.clone()));
            let label = rec.label.as_str();
            if count {
                if let Some(name) = place_name(at, places) {
                    vocab.add(Kind::Place, &name, "", "", label);
                }
                let d = &rec.taken_local;
                if let (Some(y), Some(m)) = (
                    d.get(..4).filter(|y| y.bytes().all(|b| b.is_ascii_digit())),
                    d.get(5..7).and_then(|m| m.parse::<usize>().ok()),
                ) {
                    vocab.add(Kind::Date, y, "", "", label);
                    if let Some((de, en)) = MONTHS.get(m.wrapping_sub(1)) {
                        vocab.add(Kind::Date, &format!("{de} {y}"), en, "", label);
                    }
                    if let Some((de, en)) = season(m as u32) {
                        vocab.add(Kind::Date, &format!("{de} {y}"), en, "", label);
                    }
                    if let Some((de, en)) = d
                        .get(..10)
                        .and_then(|day| chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").ok())
                        .and_then(holiday)
                    {
                        vocab.add(Kind::Date, &format!("{de} {y}"), en, "", label);
                    }
                }
                let mut kinds: Vec<u8> = objects.iter().map(|o| o.kind).collect();
                kinds.sort_unstable();
                kinds.dedup();
                for k in kinds {
                    if let Some((en, de, pl)) = crate::objects::KINDS.get(k as usize) {
                        vocab.add(Kind::Thing, de, en, pl, label);
                    }
                }
            }
            w.add_document(doc!(
                f.id => id,
                f.label => label,
                f.t => -1.0f64,
                f.text => item_words(&rec, at, places)
                    + " "
                    + &crate::objects::words(&objects),
            ))?;
            docs += 1;
            // Words said or written, once per item.
            let mut said: HashSet<String> = HashSet::new();
            if rec.video.is_none() {
                if let Some(t) = idx.text(&rec.hash)?.filter(|t| t.state == "ok") {
                    if count {
                        for l in &t.lines {
                            said.extend(crate::words::words_of(&l.text));
                        }
                    }
                    w.add_document(doc!(
                        f.id => id,
                        f.label => rec.label.as_str(),
                        f.t => READ_T,
                        f.text => t.plain(),
                    ))?;
                    docs += 1;
                }
            }
            if rec.video.is_some() {
                if let Some(tr) = idx.transcript(&rec.hash)? {
                    for p in tr.pieces {
                        if count {
                            said.extend(crate::words::words_of(&p.text));
                        }
                        w.add_document(doc!(
                            f.id => id,
                            f.label => rec.label.as_str(),
                            f.t => f64::from(p.start),
                            f.text => p.text,
                        ))?;
                        docs += 1;
                    }
                }
            }
            for word in said {
                if !crate::words::is_common(&word) {
                    vocab.add(Kind::Word, &word, "", "", label);
                }
            }
        }
        w.commit()?;
        let reader = index.reader()?;
        *self.inner.write().expect("search lock") = Some(Built {
            reader,
            index,
            f,
            docs,
            vocab: vocab.done(),
        });
        Ok(docs)
    }

    /// Up to `limit` completions of the last word of `q` (REQ-SEARCH-002):
    /// things, places, dates and words said or written that items in the
    /// libraries `labels` hold, and only those. Each is the whole query it
    /// would run; the earlier words are kept, and a query that would find
    /// nothing is not offered.
    pub fn suggest(&self, q: &str, labels: &[String], limit: usize) -> Vec<Suggestion> {
        let guard = self.inner.read().expect("search lock");
        let Some(b) = guard.as_ref() else {
            return Vec::new();
        };
        let words: Vec<&str> = q.split_whitespace().collect();
        let Some((last, head)) = words.split_last() else {
            return Vec::new();
        };
        let p = fold(last);
        if q.ends_with(char::is_whitespace) || p.chars().count() < 2 {
            return Vec::new();
        }
        let typed: Vec<String> = head.iter().map(|w| fold(w)).collect();
        let mut found: Vec<(bool, &Known, u64)> = b
            .vocab
            .iter()
            .filter(|t| t.keys.iter().any(|k| k.starts_with(&p)))
            .filter(|t| !t.keys.iter().all(|k| typed.contains(k)))
            .filter_map(|t| {
                let n: u64 =
                    t.by.iter()
                        .filter(|(l, _)| labels.contains(l))
                        .map(|(_, n)| n)
                        .sum();
                (n > 0).then(|| (fold(&t.text).starts_with(&p), t, n))
            })
            .collect();
        // What starts with the typed letters first, then by kind, then
        // the terms most items hold.
        found.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then(a.1.kind.cmp(&b.1.kind))
                .then(b.2.cmp(&a.2))
                .then(a.1.text.cmp(&b.1.text))
        });
        let candidates: Vec<(Kind, String, String, u64)> = found
            .iter()
            .take(CANDIDATES)
            .map(|(_, t, n)| (t.kind, t.text.clone(), t.hint.clone(), *n))
            .collect();
        drop(guard);
        // No kind fills the list alone while others match.
        let cap = (limit / 2).max(3);
        let mut per: HashMap<Kind, usize> = HashMap::new();
        let mut out = Vec::new();
        for (kind, term, hint, n) in candidates {
            if per.get(&kind).copied().unwrap_or(0) >= cap {
                continue;
            }
            let (query, items) = if head.is_empty() {
                (plain(&term), n)
            } else {
                // With earlier words, only what still finds something,
                // and how many items it finds.
                let query = format!("{} {}", head.join(" "), plain(&term));
                let ids: HashSet<u64> = self
                    .query(&query, labels)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|h| h.id)
                    .collect();
                if ids.is_empty() {
                    continue;
                }
                (query, ids.len() as u64)
            };
            *per.entry(kind).or_default() += 1;
            out.push(Suggestion {
                kind,
                query,
                term,
                hint,
                items,
            });
            if out.len() >= limit {
                break;
            }
        }
        out
    }

    pub fn docs(&self) -> usize {
        self.inner
            .read()
            .expect("search lock")
            .as_ref()
            .map_or(0, |b| b.docs)
    }

    /// Items matching `q` in the libraries `labels`, best first. Every
    /// word must match (in any field of one document); a word may be a
    /// prefix of at least three letters, written with `*`.
    pub fn query(&self, q: &str, labels: &[String]) -> Result<Vec<Hit>> {
        let guard = self.inner.read().expect("search lock");
        let Some(b) = guard.as_ref() else {
            return Ok(Vec::new());
        };
        if labels.is_empty() || q.trim().is_empty() {
            return Ok(Vec::new());
        }
        let mut parser = QueryParser::for_index(&b.index, vec![b.f.text]);
        parser.set_conjunction_by_default();
        let (text_q, _errors) = parser.parse_query_lenient(q);
        let scope: Vec<(Occur, Box<dyn Query>)> = labels
            .iter()
            .map(|l| {
                let q: Box<dyn Query> = Box::new(TermQuery::new(
                    Term::from_field_text(b.f.label, l),
                    IndexRecordOption::Basic,
                ));
                (Occur::Should, q)
            })
            .collect();
        let query = BooleanQuery::new(vec![
            (Occur::Must, text_q),
            (Occur::Must, Box::new(BooleanQuery::new(scope))),
        ]);
        let searcher = b.reader.searcher();
        let top = searcher.search(&query, &TopDocs::with_limit(HITS).order_by_score())?;
        let mut out: Vec<Hit> = Vec::new();
        for (_score, addr) in top {
            let d: TantivyDocument = searcher.doc(addr)?;
            let id = d.get_first(b.f.id).and_then(|v| v.as_u64()).unwrap_or(0);
            let label = d
                .get_first(b.f.label)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            // Defence in depth: never return a library outside the scope.
            if !labels.contains(&label) {
                continue;
            }
            let t = d.get_first(b.f.t).and_then(|v| v.as_f64()).unwrap_or(-1.0);
            let text = d
                .get_first(b.f.text)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let hit = match out.iter_mut().find(|h| h.id == id) {
                Some(h) => h,
                None => {
                    out.push(Hit {
                        id,
                        label,
                        speech: Vec::new(),
                        read: None,
                    });
                    out.last_mut().expect("just pushed")
                }
            };
            if t >= 0.0 && hit.speech.len() < 3 {
                hit.speech.push((t as f32, text));
            } else if t == READ_T && hit.read.is_none() {
                hit.read = Some(read_snippet(&text, q));
            }
        }
        Ok(out)
    }
}

/// The lines of `text` that hold a word of `q` (case and accents ignored
/// roughly), at most three; else the first line. At most 160 characters.
pub fn read_snippet(text: &str, q: &str) -> String {
    let words: Vec<String> = q
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| w.chars().count() >= 2)
        .collect();
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| {
            let l = l.to_lowercase();
            words.iter().any(|w| l.contains(w.as_str()))
        })
        .take(3)
        .collect();
    let s = if lines.is_empty() {
        text.lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .to_string()
    } else {
        lines.join(" … ")
    };
    match s.char_indices().nth(160) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrowing_offers_terms_that_split_the_results() {
        let t = |k: Kind, s: &str| (k, s.to_string());
        let held = vec![
            vec![t(Kind::Date, "2021"), t(Kind::Thing, "Hund")],
            vec![
                t(Kind::Date, "2021"),
                t(Kind::Thing, "Hund"),
                t(Kind::Thing, "Katze"),
            ],
            vec![t(Kind::Date, "2022"), t(Kind::Thing, "Hund")],
            vec![t(Kind::Date, "2022"), t(Kind::Place, "Neustadt, Nord")],
        ];
        let n = narrow("hund", &held);
        let got: Vec<(&str, usize)> = n.iter().map(|n| (n.term.as_str(), n.items)).collect();
        // "Hund" is typed; things first, then places, then years.
        assert_eq!(
            got,
            [
                ("Katze", 1),
                ("Neustadt, Nord", 1),
                ("2021", 2),
                ("2022", 2)
            ]
        );
        // Few results get none; a term every result holds narrows nothing.
        assert!(narrow("x", &held[..3]).is_empty());
        let same = vec![vec![t(Kind::Date, "2021")]; 5];
        assert!(narrow("x", &same).is_empty());
    }

    #[test]
    fn dates_are_found_by_season_and_holiday() {
        let w = date_words("2021-07-14T10:00:00").join(" ");
        assert!(w.contains("2021") && w.contains("Juli July") && w.contains("Sommer summer"));
        assert!(!w.contains("Ostern"), "{w}");
        assert!(date_words("2020-12-25T18:00:00")
            .join(" ")
            .contains("Weihnachten Christmas"));
        assert!(date_words("2021-01-31T08:00:00")
            .join(" ")
            .contains("Winter winter"));
        // Easter 2024 was on 31 March: Good Friday to Easter Monday.
        assert_eq!(easter(2024), chrono::NaiveDate::from_ymd_opt(2024, 3, 31));
        assert_eq!(easter(2025), chrono::NaiveDate::from_ymd_opt(2025, 4, 20));
        assert!(date_words("2024-03-29T12:00:00")
            .join(" ")
            .contains("Ostern Easter"));
        assert!(date_words("2024-04-01T12:00:00")
            .join(" ")
            .contains("Ostern Easter"));
        assert!(!date_words("2024-04-02T12:00:00")
            .join(" ")
            .contains("Ostern"));
        assert!(date_words("unknown").is_empty());
    }

    #[test]
    fn favourites_words_are_taken_out_of_a_query() {
        assert_eq!(split_fav("Favoriten Katze"), ("Katze".into(), true));
        assert_eq!(split_fav("♥ Sommer 2019"), ("Sommer 2019".into(), true));
        assert_eq!(split_fav("favourites"), (String::new(), true));
        assert_eq!(split_fav("Katze Sofa"), ("Katze Sofa".into(), false));
        assert_eq!(fav_completion("Fav"), Some("Favoriten"));
        assert_eq!(fav_completion("favou"), Some("favourites"));
        assert_eq!(fav_completion("fa"), None);
        assert_eq!(fav_completion("Katze"), None);
    }

    #[test]
    fn terms_become_plain_words_for_a_query() {
        assert_eq!(plain("Neustadt, Nord"), "Neustadt, Nord");
        assert_eq!(plain("-Ost:dorf \"x\""), "Ost dorf x");
        assert_eq!(plain("Bad Münster-Eifel"), "Bad Münster Eifel");
    }

    #[test]
    fn folding_matches_the_index() {
        assert_eq!(fold("Mädchen Straße CAFÉ"), "madchen strasse cafe");
    }
}
