//! How named people stand to each other (REQ-FACE-004): partner, parent,
//! sibling, family, friend, schoolmate, colleague, neighbour, and how much
//! each relation matters (1 to 3). Like names, relations are the same for
//! everyone in the house; a viewer sees one only between people they know.

use std::collections::HashMap;

/// A stored relation: ((a, b, kind), how much it matters).
pub type Row = ((u64, u64, u8), u8);

/// A kind of relation. Partner, sibling and the others read the same from
/// both sides and are stored with the smaller id first; Parent is stored
/// parent first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Kind {
    Partner = 0,
    Parent = 1,
    Sibling = 2,
    Family = 3,
    Friend = 4,
    Schoolmate = 5,
    Colleague = 6,
    Neighbour = 7,
}

pub const KINDS: [Kind; 8] = [
    Kind::Partner,
    Kind::Parent,
    Kind::Sibling,
    Kind::Family,
    Kind::Friend,
    Kind::Schoolmate,
    Kind::Colleague,
    Kind::Neighbour,
];

impl Kind {
    pub fn from_u8(v: u8) -> Option<Kind> {
        KINDS.get(usize::from(v)).copied()
    }

    /// How much a new relation of this kind matters until someone says.
    pub fn weight(self) -> u8 {
        match self {
            Kind::Partner | Kind::Parent | Kind::Sibling => 3,
            Kind::Family | Kind::Friend => 2,
            Kind::Schoolmate | Kind::Colleague | Kind::Neighbour => 1,
        }
    }

    /// The label read from the first person's side ("Anna, parent of Ben").
    pub fn label(self, first: bool) -> &'static str {
        match (self, first) {
            (Kind::Partner, _) => "partner of",
            (Kind::Parent, true) => "parent of",
            (Kind::Parent, false) => "child of",
            (Kind::Sibling, _) => "sibling of",
            (Kind::Family, _) => "family of",
            (Kind::Friend, _) => "friend of",
            (Kind::Schoolmate, _) => "schoolmate of",
            (Kind::Colleague, _) => "colleague of",
            (Kind::Neighbour, _) => "neighbour of",
        }
    }
}

/// The form's choices: (value, label). "child" is Parent read backwards.
pub const CHOICES: [(&str, &str); 9] = [
    ("partner", "partner of (married to)"),
    ("parent", "parent of"),
    ("child", "child of"),
    ("sibling", "sibling of"),
    ("family", "family of"),
    ("friend", "friend of"),
    ("schoolmate", "schoolmate of"),
    ("colleague", "colleague of"),
    ("neighbour", "neighbour of"),
];

/// A form value as (kind, whether the two people swap places).
pub fn parse(v: &str) -> Option<(Kind, bool)> {
    Some(match v {
        "partner" => (Kind::Partner, false),
        "parent" => (Kind::Parent, false),
        "child" => (Kind::Parent, true),
        "sibling" => (Kind::Sibling, false),
        "family" => (Kind::Family, false),
        "friend" => (Kind::Friend, false),
        "schoolmate" => (Kind::Schoolmate, false),
        "colleague" => (Kind::Colleague, false),
        "neighbour" => (Kind::Neighbour, false),
        _ => return None,
    })
}

/// The stored key of "`a` is `kind` of `b`", or None for oneself or an
/// unknown kind.
pub fn key(a: u64, b: u64, kind: u8) -> Option<(u64, u64, u8)> {
    let k = Kind::from_u8(kind)?;
    if a == b {
        return None;
    }
    Some(match k {
        Kind::Parent => (a, b, kind),
        _ => (a.min(b), a.max(b), kind),
    })
}

/// One relation seen from a person: the other person, the kind, the label
/// from this side, how much it matters.
#[derive(Debug, Clone, PartialEq)]
pub struct Seen {
    pub other: u64,
    pub kind: Kind,
    pub label: &'static str,
    pub weight: u8,
}

/// The relations of `p` with people in `known`, the ones that matter most
/// first.
pub fn of(rels: &[Row], p: u64, known: impl Fn(u64) -> bool) -> Vec<Seen> {
    let mut out: Vec<Seen> = rels
        .iter()
        .filter_map(|&((a, b, k), weight)| {
            let kind = Kind::from_u8(k)?;
            let (other, first) = if a == p {
                (b, true)
            } else if b == p {
                (a, false)
            } else {
                return None;
            };
            known(other).then(|| Seen {
                other,
                kind,
                label: kind.label(first),
                weight,
            })
        })
        .collect();
    out.sort_by(|x, y| {
        y.weight
            .cmp(&x.weight)
            .then(x.kind.cmp(&y.kind))
            .then(x.other.cmp(&y.other))
    });
    out
}

/// How close each person is to anyone the viewer knows: the weight of
/// their closest relation between two known people.
pub fn closeness(rels: &[Row], known: impl Fn(u64) -> bool) -> HashMap<u64, u8> {
    let mut out: HashMap<u64, u8> = HashMap::new();
    for &((a, b, _), w) in rels {
        if known(a) && known(b) {
            for p in [a, b] {
                let e = out.entry(p).or_default();
                *e = (*e).max(w);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_read_the_same_from_both_sides_except_parent() {
        assert_eq!(key(5, 2, Kind::Friend as u8), Some((2, 5, 4)));
        assert_eq!(key(2, 5, Kind::Friend as u8), Some((2, 5, 4)));
        assert_eq!(key(5, 2, Kind::Parent as u8), Some((5, 2, 1)));
        assert_eq!(key(3, 3, Kind::Partner as u8), None);
        assert_eq!(key(1, 2, 99), None);
        assert_eq!(parse("child"), Some((Kind::Parent, true)));
        assert_eq!(parse("boss"), None);
    }

    #[test]
    fn a_persons_relations_closest_first_and_only_known_people() {
        let rels = vec![
            ((1, 2, Kind::Schoolmate as u8), 1),
            ((3, 1, Kind::Parent as u8), 3),
            ((1, 4, Kind::Partner as u8), 3),
            ((1, 9, Kind::Friend as u8), 2),
        ];
        let seen = of(&rels, 1, |p| p != 9);
        let got: Vec<(u64, &str, u8)> = seen.iter().map(|s| (s.other, s.label, s.weight)).collect();
        assert_eq!(
            got,
            vec![
                (4, "partner of", 3),
                (3, "child of", 3),
                (2, "schoolmate of", 1)
            ]
        );
        let c = closeness(&rels, |p| p != 9);
        assert_eq!(c.get(&1), Some(&3));
        assert_eq!(c.get(&2), Some(&1));
        assert_eq!(c.get(&9), None);
    }
}
