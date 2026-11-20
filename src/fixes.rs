//! Corrections (REQ-FIX-001, REQ-FIX-002, DD-026): what the house says the
//! looking got wrong. A thing found can be marked right, wrong or another
//! kind; a kind can be "never in our house"; a line of text read can be
//! corrected or taken out. Corrections are kept apart from what the models
//! found, so the next look pass (a new model) does not bring a mistake
//! back: they are matched to its finds by kind and box. Wrong finds also
//! raise the bar for their kind, so the same mistake is not made in other
//! photos. They are the household's, shown only with photos one may see,
//! and can be taken out as a file to teach a model one day.

use crate::objects::{Object, KINDS, MIN_SCORE};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Boxes this close are the same find (intersection over union).
pub const SAME: f32 = 0.5;
/// A kind's bar is raised from this many wrong finds on.
pub const WRONG_TO_LEARN: usize = 2;
/// The bar is never raised above this.
pub const BAR_MAX: f32 = 0.95;

/// What someone said about a thing found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThingFix {
    /// The kind the model said, its box and its score.
    pub kind: u8,
    pub bbox: [f32; 4],
    pub score: f32,
    /// None: it is not there. Some(kind): it is that kind (the same kind
    /// means the model was right).
    pub now: Option<u8>,
    pub by: String,
    pub at: u64,
}

impl ThingFix {
    pub fn right(&self) -> bool {
        self.now == Some(self.kind)
    }
}

/// What someone said about a line of text read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextFix {
    /// The line's box (the bounds of its quad) and what was read.
    pub bbox: [f32; 4],
    pub was: String,
    /// None: there is no such text. Some: what it says.
    pub now: Option<String>,
    pub by: String,
    pub at: u64,
}

/// Who said a kind is never in the house, and when.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mark {
    pub by: String,
    pub at: u64,
}

/// Intersection over union of two boxes (x0, y0, x1, y1).
pub fn overlap(a: [f32; 4], b: [f32; 4]) -> f32 {
    let w = (a[2].min(b[2]) - a[0].max(b[0])).max(0.0);
    let h = (a[3].min(b[3]) - a[1].max(b[1])).max(0.0);
    let i = w * h;
    let area = |r: [f32; 4]| (r[2] - r[0]).max(0.0) * (r[3] - r[1]).max(0.0);
    let u = area(a) + area(b) - i;
    if u <= 0.0 {
        0.0
    } else {
        i / u
    }
}

/// The bounds of a text line's quad.
pub fn quad_box(q: &[f32; 8]) -> [f32; 4] {
    let xs = [q[0], q[2], q[4], q[6]];
    let ys = [q[1], q[3], q[5], q[7]];
    let min = |v: [f32; 4]| v.iter().copied().fold(f32::MAX, f32::min);
    let max = |v: [f32; 4]| v.iter().copied().fold(f32::MIN, f32::max);
    [min(xs), min(ys), max(xs), max(ys)]
}

/// Every correction of things, read once and applied to many photos.
#[derive(Debug, Clone, Default)]
pub struct Fixes {
    pub things: HashMap<String, Vec<ThingFix>>,
    pub off: HashMap<u8, Mark>,
    bar: HashMap<u8, f32>,
}

impl Fixes {
    pub fn new(things: HashMap<String, Vec<ThingFix>>, off: HashMap<u8, Mark>) -> Fixes {
        let bar = bars(things.values().flatten());
        Fixes { things, off, bar }
    }

    /// The score a find of `kind` needs to be kept.
    pub fn bar(&self, kind: u8) -> f32 {
        self.bar.get(&kind).copied().unwrap_or(MIN_SCORE)
    }

    /// The correction that speaks about `o` in the photo `hash`.
    pub fn about(&self, hash: &str, o: &Object) -> Option<&ThingFix> {
        about(self.things.get(hash)?, o)
    }

    /// Whether `o` is kept, and as which kind: a word about it wins; then a
    /// kind never in the house and the kind's bar leave it out.
    pub fn verdict(&self, hash: &str, o: &Object) -> Option<u8> {
        match self.about(hash, o) {
            Some(f) => f.now,
            None if self.off.contains_key(&o.kind) || o.score < self.bar(o.kind) => None,
            None => Some(o.kind),
        }
    }

    /// The things in the photo `hash` as the house says they are.
    pub fn apply(&self, hash: &str, objects: Vec<Object>) -> Vec<Object> {
        objects
            .into_iter()
            .filter_map(|mut o| {
                o.kind = self.verdict(hash, &o)?;
                Some(o)
            })
            .collect()
    }

    /// How many of `objects` are left out only by the raised bar.
    pub fn below_bar(&self, hash: &str, objects: &[Object]) -> Vec<u8> {
        objects
            .iter()
            .filter(|o| {
                self.about(hash, o).is_none()
                    && !self.off.contains_key(&o.kind)
                    && o.score < self.bar(o.kind)
            })
            .map(|o| o.kind)
            .collect()
    }
}

/// The correction among `fixes` that speaks about `o`: the same kind as
/// the model said, the box closest and at least `SAME`.
pub fn about<'a>(fixes: &'a [ThingFix], o: &Object) -> Option<&'a ThingFix> {
    fixes
        .iter()
        .filter(|f| f.kind == o.kind)
        .map(|f| (overlap(f.bbox, o.bbox), f))
        .filter(|(v, _)| *v >= SAME)
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, f)| f)
}

/// The bar per kind, learned from what was marked: from `WRONG_TO_LEARN`
/// wrong finds of a kind on, it needs more than the surest of them, but
/// never more than the least sure find marked right, and at most
/// `BAR_MAX`.
pub fn bars<'a>(fixes: impl Iterator<Item = &'a ThingFix>) -> HashMap<u8, f32> {
    let mut wrong: HashMap<u8, Vec<f32>> = HashMap::new();
    let mut right: HashMap<u8, f32> = HashMap::new();
    for f in fixes {
        if f.right() {
            let r = right.entry(f.kind).or_insert(f.score);
            *r = r.min(f.score);
        } else {
            wrong.entry(f.kind).or_default().push(f.score);
        }
    }
    wrong
        .into_iter()
        .filter(|(_, s)| s.len() >= WRONG_TO_LEARN)
        .map(|(k, s)| {
            let top = s.iter().copied().fold(0.0, f32::max) + 0.01;
            let mut bar = top.min(BAR_MAX);
            if let Some(r) = right.get(&k) {
                bar = bar.min(*r);
            }
            (k, bar.max(MIN_SCORE))
        })
        .collect()
}

/// Put a word about a find among `fixes`, replacing any earlier word
/// about the same find.
pub fn put(fixes: &mut Vec<ThingFix>, fix: ThingFix) {
    let probe = Object {
        kind: fix.kind,
        score: fix.score,
        bbox: fix.bbox,
    };
    fixes.retain(|f| !(f.kind == fix.kind && overlap(f.bbox, probe.bbox) >= SAME));
    fixes.push(fix);
}

/// The text read in a photo as the house says it is: a line marked wrong
/// is left out, a corrected line says what it was corrected to.
pub fn apply_text(mut text: crate::ocr::Text, fixes: &[TextFix]) -> crate::ocr::Text {
    if fixes.is_empty() {
        return text;
    }
    text.lines = text
        .lines
        .into_iter()
        .filter_map(|mut l| {
            let b = quad_box(&l.quad);
            match text_about(fixes, b, &l.text) {
                Some(f) => {
                    l.text = f.now.clone()?;
                    Some(l)
                }
                None => Some(l),
            }
        })
        .collect();
    if text.lines.is_empty() {
        text.state = "none".into();
    }
    text
}

/// The correction among `fixes` about the line at `b` that read `was`:
/// the same text, else the box closest and at least `SAME`.
pub fn text_about<'a>(fixes: &'a [TextFix], b: [f32; 4], was: &str) -> Option<&'a TextFix> {
    fixes
        .iter()
        .map(|f| (overlap(f.bbox, b), f))
        .filter(|(v, f)| *v >= SAME || (f.was == was && *v > 0.0))
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, f)| f)
}

/// The name of a kind in the house's language, singular.
pub fn kind_name(k: u8) -> &'static str {
    KINDS.get(k as usize).map_or("?", |t| t.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(kind: u8, score: f32, bbox: [f32; 4]) -> Object {
        Object { kind, score, bbox }
    }

    fn fix(kind: u8, score: f32, bbox: [f32; 4], now: Option<u8>) -> ThingFix {
        ThingFix {
            kind,
            bbox,
            score,
            now,
            by: "a".into(),
            at: 1,
        }
    }

    const BOARD: u8 = 31;
    const SKIS: u8 = 30;
    const BOX: [f32; 4] = [0.1, 0.1, 0.5, 0.5];

    #[test]
    fn a_word_about_a_find_holds_for_the_next_model_too() {
        let mut things = HashMap::new();
        things.insert("h".to_string(), vec![fix(BOARD, 0.8, BOX, None)]);
        let fx = Fixes::new(things, HashMap::new());
        // The next model finds it a little elsewhere: still the same find.
        let again = obj(BOARD, 0.82, [0.12, 0.1, 0.52, 0.5]);
        let other = obj(BOARD, 0.9, [0.6, 0.6, 0.9, 0.9]);
        let kept = fx.apply("h", vec![again, other.clone()]);
        assert_eq!(kept, vec![other]);
        // Another photo is not touched by it.
        assert_eq!(fx.apply("g", vec![obj(BOARD, 0.9, BOX)]).len(), 1);
    }

    #[test]
    fn a_find_can_be_another_kind_and_a_right_find_stays() {
        let mut things = HashMap::new();
        things.insert(
            "h".to_string(),
            vec![
                fix(BOARD, 0.8, BOX, Some(SKIS)),
                fix(0, 0.71, [0.6, 0.0, 1.0, 1.0], Some(0)),
            ],
        );
        let mut off = HashMap::new();
        off.insert(
            0,
            Mark {
                by: "a".into(),
                at: 1,
            },
        );
        let fx = Fixes::new(things, off);
        let kept = fx.apply(
            "h",
            vec![obj(BOARD, 0.8, BOX), obj(0, 0.71, [0.6, 0.0, 1.0, 1.0])],
        );
        assert_eq!(
            kept.iter().map(|o| o.kind).collect::<Vec<_>>(),
            vec![SKIS, 0]
        );
        // A kind never in the house is gone where nobody said it is right.
        assert!(fx.apply("g", vec![obj(0, 0.99, BOX)]).is_empty());
    }

    #[test]
    fn wrong_finds_raise_the_bar_for_their_kind_but_not_over_a_right_one() {
        let one = [fix(BOARD, 0.8, BOX, None)];
        assert!(bars(one.iter()).is_empty(), "one tap alone does not learn");
        let two = vec![
            fix(BOARD, 0.8, BOX, None),
            fix(BOARD, 0.84, BOX, Some(SKIS)),
        ];
        let b = bars(two.iter())[&BOARD];
        assert!((b - 0.85).abs() < 1e-6, "{b}");
        let mut three = two.clone();
        three.push(fix(BOARD, 0.75, BOX, Some(BOARD)));
        assert!((bars(three.iter())[&BOARD] - 0.75).abs() < 1e-6);
        let high = [fix(SKIS, 0.99, BOX, None), fix(SKIS, 0.99, BOX, None)];
        assert!((bars(high.iter())[&SKIS] - BAR_MAX).abs() < 1e-6);
        let mut things = HashMap::new();
        things.insert("h".to_string(), two);
        let fx = Fixes::new(things, HashMap::new());
        assert!(fx.apply("g", vec![obj(BOARD, 0.84, BOX)]).is_empty());
        assert_eq!(fx.apply("g", vec![obj(BOARD, 0.86, BOX)]).len(), 1);
        assert_eq!(fx.below_bar("g", &[obj(BOARD, 0.84, BOX)]), vec![BOARD]);
    }

    #[test]
    fn a_later_word_replaces_an_earlier_one() {
        let mut v = vec![fix(BOARD, 0.8, BOX, None)];
        put(&mut v, fix(BOARD, 0.8, BOX, Some(BOARD)));
        put(&mut v, fix(BOARD, 0.8, [0.6, 0.6, 0.9, 0.9], None));
        assert_eq!(v.len(), 2);
        assert!(v[0].right());
    }

    #[test]
    fn a_line_of_text_is_corrected_or_taken_out() {
        use crate::ocr::{Line, Text};
        let line = |t: &str, y: f32| Line {
            quad: [0.1, y, 0.9, y, 0.9, y + 0.05, 0.1, y + 0.05],
            block: 0,
            text: t.into(),
            conf: 0.9,
        };
        let text = Text {
            state: "ok".into(),
            model: "m".into(),
            lines: vec![line("Hcllo", 0.1), line("~~~", 0.5)],
            docs: vec![0],
        };
        let fixes = vec![
            TextFix {
                bbox: [0.1, 0.1, 0.9, 0.15],
                was: "Hcllo".into(),
                now: Some("Hallo".into()),
                by: "a".into(),
                at: 1,
            },
            TextFix {
                bbox: [0.1, 0.5, 0.9, 0.55],
                was: "~~~".into(),
                now: None,
                by: "a".into(),
                at: 1,
            },
        ];
        let t = apply_text(text, &fixes);
        assert_eq!(t.lines.len(), 1);
        assert_eq!(t.lines[0].text, "Hallo");
        assert_eq!(t.state, "ok");
    }
}
