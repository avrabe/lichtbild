//! One search box (REQ-SEARCH-001, DD-019): what was said in videos, place
//! names, dates, file and folder names. A tantivy index in memory, rebuilt
//! from the redb index after each scan and transcription pass; nothing is
//! written to disk. Results are scoped to the libraries the viewer may see
//! (REQ-AUTH-002) inside the query, not after it.

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
    if let (Some(p), Some((lat, lon))) = (places, at) {
        if let Some(n) = p.nearest(lat, lon) {
            words.push(n.label);
        }
    }
    let d = &rec.taken_local;
    if let (Some(y), Some(m)) = (
        d.get(..4),
        d.get(5..7).and_then(|m| m.parse::<usize>().ok()),
    ) {
        words.push(y.to_string());
        if let Some((de, en)) = MONTHS.get(m.wrapping_sub(1)) {
            words.push(format!("{de} {en}"));
        }
        if let Some(day) = d.get(..10) {
            words.push(day.to_string());
        }
    }
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
        for (id, rec) in items {
            if rec.missing {
                continue;
            }
            w.add_document(doc!(
                f.id => id,
                f.label => rec.label.as_str(),
                f.t => -1.0f64,
                f.text => item_words(&rec, pos.get(&id).map(|p| (p.lat, p.lon)), places)
                    + " "
                    + &crate::objects::words(&idx.objects(&rec.hash)?),
            ))?;
            docs += 1;
            if rec.video.is_none() {
                if let Some(t) = idx.text(&rec.hash)?.filter(|t| t.state == "ok") {
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
        }
        w.commit()?;
        let reader = index.reader()?;
        *self.inner.write().expect("search lock") = Some(Built {
            reader,
            index,
            f,
            docs,
        });
        Ok(docs)
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
