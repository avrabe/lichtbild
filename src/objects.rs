//! Things in pictures (REQ-SEE-001, DD-024): RT-DETRv2-R18 finds the 80
//! COCO kinds of thing (people, animals, vehicles, food, furniture…) with
//! their boxes, run by rten in the `look` child. Labels are kept in
//! English and German, so search finds "Hund" and "dog".

use anyhow::{bail, Result};
use image::RgbImage;
use rten_tensor::prelude::*;
use rten_tensor::{NdTensor, Tensor};
use serde::{Deserialize, Serialize};

/// The detector's input side.
const SIDE: u32 = 640;
/// Kept from this score on (DD-024: noise patterns reached 0.63).
pub const MIN_SCORE: f32 = 0.7;
/// A box this close to the whole picture is a texture, not a thing, unless
/// the detector is very sure (DD-024: noise patterns gave whole-picture
/// "trains" at 0.71–0.92).
const WHOLE: f32 = 0.97;
const WHOLE_SURE: f32 = 0.95;

/// Whether a find is kept.
fn plausible(score: f32, bbox: [f32; 4]) -> bool {
    let whole = bbox[2] - bbox[0] >= WHOLE && bbox[3] - bbox[1] >= WHOLE;
    score >= MIN_SCORE && (!whole || score >= WHOLE_SURE)
}

/// At most this many things per picture.
const MAX: usize = 40;

/// The 80 COCO kinds: (English, German singular, German plural).
pub const KINDS: [(&str, &str, &str); 80] = [
    ("person", "Person", "Personen"),
    ("bicycle", "Fahrrad", "Fahrräder"),
    ("car", "Auto", "Autos"),
    ("motorbike", "Motorrad", "Motorräder"),
    ("aeroplane", "Flugzeug", "Flugzeuge"),
    ("bus", "Bus", "Busse"),
    ("train", "Zug", "Züge"),
    ("truck", "Lastwagen", "Lastwagen"),
    ("boat", "Boot", "Boote"),
    ("traffic light", "Ampel", "Ampeln"),
    ("fire hydrant", "Hydrant", "Hydranten"),
    ("stop sign", "Stoppschild", "Stoppschilder"),
    ("parking meter", "Parkuhr", "Parkuhren"),
    ("bench", "Bank", "Bänke"),
    ("bird", "Vogel", "Vögel"),
    ("cat", "Katze", "Katzen"),
    ("dog", "Hund", "Hunde"),
    ("horse", "Pferd", "Pferde"),
    ("sheep", "Schaf", "Schafe"),
    ("cow", "Kuh", "Kühe"),
    ("elephant", "Elefant", "Elefanten"),
    ("bear", "Bär", "Bären"),
    ("zebra", "Zebra", "Zebras"),
    ("giraffe", "Giraffe", "Giraffen"),
    ("backpack", "Rucksack", "Rucksäcke"),
    ("umbrella", "Regenschirm", "Regenschirme"),
    ("handbag", "Handtasche", "Handtaschen"),
    ("tie", "Krawatte", "Krawatten"),
    ("suitcase", "Koffer", "Koffer"),
    ("frisbee", "Frisbee", "Frisbees"),
    ("skis", "Ski", "Ski"),
    ("snowboard", "Snowboard", "Snowboards"),
    ("sports ball", "Ball", "Bälle"),
    ("kite", "Drachen", "Drachen"),
    ("baseball bat", "Baseballschläger", "Baseballschläger"),
    ("baseball glove", "Baseballhandschuh", "Baseballhandschuhe"),
    ("skateboard", "Skateboard", "Skateboards"),
    ("surfboard", "Surfbrett", "Surfbretter"),
    ("tennis racket", "Tennisschläger", "Tennisschläger"),
    ("bottle", "Flasche", "Flaschen"),
    ("wine glass", "Weinglas", "Weingläser"),
    ("cup", "Tasse", "Tassen"),
    ("fork", "Gabel", "Gabeln"),
    ("knife", "Messer", "Messer"),
    ("spoon", "Löffel", "Löffel"),
    ("bowl", "Schüssel", "Schüsseln"),
    ("banana", "Banane", "Bananen"),
    ("apple", "Apfel", "Äpfel"),
    ("sandwich", "Sandwich", "Sandwiches"),
    ("orange", "Orange", "Orangen"),
    ("broccoli", "Brokkoli", "Brokkoli"),
    ("carrot", "Karotte", "Karotten"),
    ("hot dog", "Hotdog", "Hotdogs"),
    ("pizza", "Pizza", "Pizzen"),
    ("donut", "Donut", "Donuts"),
    ("cake", "Kuchen", "Kuchen"),
    ("chair", "Stuhl", "Stühle"),
    ("sofa", "Sofa", "Sofas"),
    ("pottedplant", "Topfpflanze", "Topfpflanzen"),
    ("bed", "Bett", "Betten"),
    ("diningtable", "Tisch", "Tische"),
    ("toilet", "Toilette", "Toiletten"),
    ("tvmonitor", "Fernseher", "Fernseher"),
    ("laptop", "Laptop", "Laptops"),
    ("mouse", "Maus", "Mäuse"),
    ("remote", "Fernbedienung", "Fernbedienungen"),
    ("keyboard", "Tastatur", "Tastaturen"),
    ("cell phone", "Handy", "Handys"),
    ("microwave", "Mikrowelle", "Mikrowellen"),
    ("oven", "Ofen", "Öfen"),
    ("toaster", "Toaster", "Toaster"),
    ("sink", "Waschbecken", "Waschbecken"),
    ("refrigerator", "Kühlgerät", "Kühlgeräte"),
    ("book", "Buch", "Bücher"),
    ("clock", "Uhr", "Uhren"),
    ("vase", "Vase", "Vasen"),
    ("scissors", "Schere", "Scheren"),
    ("teddy bear", "Teddybär", "Teddybären"),
    ("hair drier", "Föhn", "Föhne"),
    ("toothbrush", "Zahnbürste", "Zahnbürsten"),
];

/// One thing found: its kind (index into KINDS), the detector's score,
/// and its box as fractions of the picture (x0, y0, x1, y1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Object {
    pub kind: u8,
    pub score: f32,
    pub bbox: [f32; 4],
}

impl Object {
    pub fn name(&self) -> &'static str {
        KINDS.get(self.kind as usize).map_or("?", |k| k.1)
    }
}

/// The words search should know a picture by: each kind once, in German
/// (singular and plural) and English.
pub fn words(objects: &[Object]) -> String {
    let mut kinds: Vec<u8> = objects.iter().map(|o| o.kind).collect();
    kinds.sort_unstable();
    kinds.dedup();
    kinds
        .iter()
        .filter_map(|k| KINDS.get(*k as usize))
        .map(|(en, de, pl)| format!("{de} {pl} {en}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The detector.
pub struct Detector {
    model: rten::Model,
}

impl Detector {
    pub fn new(model: rten::Model) -> Detector {
        Detector { model }
    }

    /// The things in `img`, best first.
    pub fn detect(&self, img: &RgbImage) -> Result<Vec<Object>> {
        let r = image::imageops::resize(img, SIDE, SIDE, image::imageops::FilterType::Triangle);
        let s = SIDE as usize;
        let mut t = NdTensor::<f32, 4>::zeros([1, 3, s, s]);
        for (x, y, p) in r.enumerate_pixels() {
            for c in 0..3 {
                t[[0, c, y as usize, x as usize]] = p[c] as f32 / 255.0;
            }
        }
        let input = self.model.node_id("pixel_values")?;
        let (lid, bid) = (
            self.model.node_id("logits")?,
            self.model.node_id("pred_boxes")?,
        );
        let mut out = self
            .model
            .run(vec![(input, t.view().into())], &[lid, bid], None)?
            .into_iter();
        let (Some(logits), Some(boxes)) = (out.next(), out.next()) else {
            bail!("detector output");
        };
        let logits: Tensor<f32> = logits.try_into()?;
        let boxes: Tensor<f32> = boxes.try_into()?;
        let shape = logits.shape().to_vec();
        let [_, queries, kinds] = shape[..] else {
            bail!("detector logits {shape:?}");
        };
        if kinds != KINDS.len() {
            bail!("detector knows {kinds} kinds, not {}", KINDS.len());
        }
        let logits = logits.into_shape([queries, kinds]);
        let boxes = boxes.into_shape([queries, 4]);
        let mut found = Vec::new();
        for q in 0..queries {
            let (mut best, mut kind) = (f32::MIN, 0);
            for k in 0..kinds {
                if logits[[q, k]] > best {
                    best = logits[[q, k]];
                    kind = k;
                }
            }
            let score = 1.0 / (1.0 + (-best).exp());
            // Centre and size, as fractions of the picture.
            let (cx, cy, w, h) = (boxes[[q, 0]], boxes[[q, 1]], boxes[[q, 2]], boxes[[q, 3]]);
            let c = |v: f32| v.clamp(0.0, 1.0);
            let bbox = [
                c(cx - w / 2.0),
                c(cy - h / 2.0),
                c(cx + w / 2.0),
                c(cy + h / 2.0),
            ];
            if !plausible(score, bbox) {
                continue;
            }
            found.push(Object {
                kind: kind as u8,
                score,
                bbox: [
                    c(cx - w / 2.0),
                    c(cy - h / 2.0),
                    c(cx + w / 2.0),
                    c(cy + h / 2.0),
                ],
            });
        }
        found.sort_by(|a, b| b.score.total_cmp(&a.score));
        found.truncate(MAX);
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_picture_boxes_need_a_sure_find() {
        assert!(!plausible(0.92, [0.0, 0.0, 1.0, 1.0]), "a texture");
        assert!(plausible(0.97, [0.0, 0.0, 1.0, 1.0]), "a sure close-up");
        assert!(plausible(0.72, [0.1, 0.2, 0.6, 0.9]));
        assert!(!plausible(0.65, [0.1, 0.2, 0.6, 0.9]), "unsure");
    }

    #[test]
    fn search_words_name_each_kind_once_in_both_languages() {
        let o = |kind: u8| Object {
            kind,
            score: 0.9,
            bbox: [0.0, 0.0, 1.0, 1.0],
        };
        assert_eq!(
            words(&[o(16), o(0), o(16)]),
            "Person Personen person Hund Hunde dog"
        );
        assert_eq!(o(15).name(), "Katze");
    }
}
