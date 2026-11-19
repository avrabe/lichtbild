//! Words (REQ-WORDS-001): what was said in the viewer's videos, as a cloud
//! of the words most videos share. Common German and English words are
//! left out, so what remains are names, places and things. Read from the
//! stored transcripts, scoped to the libraries the viewer may see.

use std::collections::{HashMap, HashSet};

use anyhow::Result;

use crate::index::Index;

/// Words too common to say anything about a video (German and English),
/// lower case. Shorter words (under four letters) are dropped anyway.
const STOP: &[&str] = &[
    // German
    "aber",
    "alle",
    "allem",
    "allen",
    "aller",
    "alles",
    "also",
    "andere",
    "anderen",
    "auch",
    "bein",
    "beim",
    "bist",
    "bitte",
    "bloß",
    "bzw",
    "dabei",
    "dafür",
    "daher",
    "damals",
    "damit",
    "dann",
    "darauf",
    "darf",
    "darfst",
    "darum",
    "dass",
    "davon",
    "dazu",
    "denn",
    "dein",
    "deine",
    "deinen",
    "deiner",
    "deines",
    "dem",
    "denen",
    "derer",
    "deren",
    "dessen",
    "dich",
    "dies",
    "diese",
    "diesem",
    "diesen",
    "dieser",
    "dieses",
    "doch",
    "dort",
    "durch",
    "eben",
    "ebenso",
    "eine",
    "einem",
    "einen",
    "einer",
    "eines",
    "einfach",
    "einige",
    "einmal",
    "erst",
    "etwa",
    "etwas",
    "euch",
    "euer",
    "eure",
    "fast",
    "für",
    "ganz",
    "gar",
    "gegen",
    "geht",
    "gehen",
    "gemacht",
    "genau",
    "gerade",
    "gern",
    "gerne",
    "gewesen",
    "gibt",
    "gleich",
    "glaube",
    "habe",
    "haben",
    "habt",
    "hast",
    "hatte",
    "hatten",
    "hätte",
    "hätten",
    "heute",
    "hier",
    "hinter",
    "ihnen",
    "ihre",
    "ihrem",
    "ihren",
    "ihrer",
    "ihres",
    "immer",
    "indem",
    "jede",
    "jedem",
    "jeden",
    "jeder",
    "jedes",
    "jetzt",
    "kann",
    "kannst",
    "kein",
    "keine",
    "keinen",
    "keiner",
    "können",
    "könnte",
    "machen",
    "macht",
    "mache",
    "mal",
    "manchmal",
    "mehr",
    "mein",
    "meine",
    "meinem",
    "meinen",
    "meiner",
    "mich",
    "mir",
    "mit",
    "muss",
    "musst",
    "müssen",
    "nach",
    "nachdem",
    "nein",
    "nicht",
    "nichts",
    "noch",
    "nun",
    "nur",
    "oben",
    "oder",
    "ohne",
    "okay",
    "schon",
    "sehr",
    "sein",
    "seine",
    "seinem",
    "seinen",
    "seiner",
    "selbst",
    "sich",
    "sind",
    "soll",
    "sollen",
    "sollte",
    "sondern",
    "sonst",
    "über",
    "überhaupt",
    "unser",
    "unsere",
    "unter",
    "viel",
    "viele",
    "vielleicht",
    "voll",
    "vom",
    "von",
    "vor",
    "wann",
    "war",
    "waren",
    "warst",
    "warum",
    "was",
    "weil",
    "weiter",
    "welche",
    "welchen",
    "welcher",
    "wenn",
    "werde",
    "werden",
    "wieder",
    "will",
    "willst",
    "wird",
    "wirklich",
    "wirst",
    "wohl",
    "wollen",
    "wollte",
    "würde",
    "würden",
    "zum",
    "zur",
    "zwar",
    "zwischen",
    "also",
    "genau",
    "jetzt",
    "komm",
    "komme",
    "kommen",
    "kommt",
    "guck",
    "gucken",
    "guckt",
    "schau",
    "schauen",
    "sagen",
    "sagt",
    "gesagt",
    "weiß",
    "wissen",
    "mach",
    "ja",
    "nee",
    "naja",
    "halt",
    "eigentlich",
    // English
    "about",
    "after",
    "again",
    "also",
    "back",
    "because",
    "been",
    "before",
    "being",
    "come",
    "could",
    "does",
    "doing",
    "done",
    "down",
    "each",
    "even",
    "from",
    "gets",
    "give",
    "goes",
    "going",
    "gonna",
    "good",
    "got",
    "have",
    "having",
    "here",
    "into",
    "just",
    "know",
    "like",
    "look",
    "make",
    "many",
    "more",
    "much",
    "must",
    "need",
    "only",
    "okay",
    "other",
    "over",
    "really",
    "right",
    "said",
    "same",
    "says",
    "should",
    "some",
    "such",
    "sure",
    "take",
    "tell",
    "than",
    "that",
    "their",
    "them",
    "then",
    "there",
    "these",
    "they",
    "thing",
    "think",
    "this",
    "those",
    "through",
    "very",
    "wanna",
    "want",
    "well",
    "were",
    "what",
    "when",
    "where",
    "which",
    "while",
    "will",
    "with",
    "would",
    "yeah",
    "your",
    "yours",
    "you're",
    "it's",
    "that's",
    "don't",
    "didn't",
    "can't",
    "i'm",
    "let's",
    "there's",
    "what's",
    "come",
    "came",
    "let",
];

/// One word of the cloud: how many videos say it, how often in all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    pub word: String,
    pub videos: u64,
    pub times: u64,
}

fn words_of(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '-'))
        .map(|w| w.trim_matches(|c| c == '\'' || c == '-').to_lowercase())
        .filter(|w| {
            w.chars().count() >= 4
                && w.chars()
                    .all(|c| c.is_alphabetic() || c == '-' || c == '\'')
        })
}

/// The `limit` words most videos share, in the libraries `labels`.
pub fn cloud(idx: &Index, labels: &[String], limit: usize) -> Result<Vec<Word>> {
    let stop: HashSet<&str> = STOP.iter().copied().collect();
    let mut videos: HashMap<String, u64> = HashMap::new();
    let mut times: HashMap<String, u64> = HashMap::new();
    let mut seen_hash = HashSet::new();
    for (_, rec) in idx.all()? {
        if rec.missing || rec.video.is_none() || !labels.contains(&rec.label) {
            continue;
        }
        // A video in two libraries (or twice) counts once.
        if !seen_hash.insert(rec.hash.clone()) {
            continue;
        }
        let Some(tr) = idx.transcript(&rec.hash)? else {
            continue;
        };
        let mut here = HashSet::new();
        for p in &tr.pieces {
            for w in words_of(&p.text) {
                if stop.contains(w.as_str()) {
                    continue;
                }
                *times.entry(w.clone()).or_default() += 1;
                here.insert(w);
            }
        }
        for w in here {
            *videos.entry(w).or_default() += 1;
        }
    }
    let mut out: Vec<Word> = videos
        .into_iter()
        .map(|(word, v)| Word {
            times: times.get(&word).copied().unwrap_or(0),
            word,
            videos: v,
        })
        .collect();
    out.sort_by(|a, b| {
        (b.videos, b.times)
            .cmp(&(a.videos, a.times))
            .then_with(|| a.word.cmp(&b.word))
    });
    out.truncate(limit);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_are_split_and_short_or_numeric_ones_dropped() {
        let w: Vec<String> =
            words_of("Heute: der Zirkus, 2024 Kunstreiterin's Ball-spiel ok").collect();
        assert_eq!(w, ["heute", "zirkus", "kunstreiterin's", "ball-spiel"]);
    }

    #[test]
    fn stop_words_are_lower_case() {
        assert!(STOP.iter().all(|w| w.to_lowercase() == *w));
    }
}
