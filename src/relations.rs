//! How named people stand to each other (REQ-FACE-004): partner, parent,
//! grandparent, sibling, cousin, in-law, friend, schoolmate and more, and how much
//! each relation matters (1 to 3). Like names, relations are the same for
//! everyone in the house; a viewer sees one only between people they know.

use std::collections::HashMap;

/// A stored relation: ((a, b, kind), how much it matters).
pub type Row = ((u64, u64, u8), u8);

/// A kind of relation, stored as its number (never renumber; new kinds go
/// at the end). Directed kinds (parent, grandparent, aunt or uncle,
/// godparent, teacher) are stored with the elder first; the others read
/// the same from both sides and are stored with the smaller id first.
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
    Grandparent = 8,
    AuntUncle = 9,
    Cousin = 10,
    InLaw = 11,
    Godparent = 12,
    ExPartner = 13,
    Teacher = 14,
    Teammate = 15,
    Acquaintance = 16,
}

/// Every kind: (kind, form value and label from the first side, from the
/// second side for a directed kind, default weight). The order is the
/// form's.
/// One kind: (kind, (value, label), the other side, default weight).
type Def = (
    Kind,
    (&'static str, &'static str),
    Option<(&'static str, &'static str)>,
    u8,
);

const DEFS: [Def; 17] = [
    (Kind::Partner, ("partner", "partner of"), None, 3),
    (
        Kind::Parent,
        ("parent", "parent of"),
        Some(("child", "child of")),
        3,
    ),
    (Kind::Sibling, ("sibling", "sibling of"), None, 3),
    (
        Kind::Grandparent,
        ("grandparent", "grandparent of"),
        Some(("grandchild", "grandchild of")),
        3,
    ),
    (
        Kind::AuntUncle,
        ("auntuncle", "aunt or uncle of"),
        Some(("niecenephew", "niece or nephew of")),
        2,
    ),
    (Kind::Cousin, ("cousin", "cousin of"), None, 2),
    (Kind::InLaw, ("inlaw", "in-law of"), None, 2),
    (
        Kind::Godparent,
        ("godparent", "godparent of"),
        Some(("godchild", "godchild of")),
        2,
    ),
    (Kind::Family, ("family", "family of"), None, 2),
    (Kind::Friend, ("friend", "friend of"), None, 2),
    (Kind::ExPartner, ("expartner", "former partner of"), None, 1),
    (Kind::Schoolmate, ("schoolmate", "schoolmate of"), None, 1),
    (
        Kind::Teacher,
        ("teacher", "teacher of"),
        Some(("student", "student of")),
        1,
    ),
    (Kind::Colleague, ("colleague", "colleague of"), None, 1),
    (
        Kind::Teammate,
        ("teammate", "teammate of (club, team)"),
        None,
        1,
    ),
    (Kind::Neighbour, ("neighbour", "neighbour of"), None, 1),
    (
        Kind::Acquaintance,
        ("acquaintance", "acquaintance of"),
        None,
        1,
    ),
];

impl Kind {
    pub fn from_u8(v: u8) -> Option<Kind> {
        DEFS.iter().map(|d| d.0).find(|k| *k as u8 == v)
    }

    fn def(self) -> &'static Def {
        DEFS.iter()
            .find(|d| d.0 == self)
            .expect("every kind has a definition")
    }

    /// Whether the kind reads differently from the two sides.
    pub fn directed(self) -> bool {
        self.def().2.is_some()
    }

    /// How much a new relation of this kind matters until someone says.
    pub fn weight(self) -> u8 {
        self.def().3
    }

    /// The label read from the first person's side ("Anna, parent of Ben").
    pub fn label(self, first: bool) -> &'static str {
        let d = self.def();
        match (first, d.2) {
            (false, Some((_, l))) => l,
            _ => d.1 .1,
        }
    }
}

/// The form's choices: (value, label), both sides of a directed kind.
pub fn choices() -> Vec<(&'static str, &'static str)> {
    let mut out = Vec::new();
    for (k, first, second, _) in DEFS.iter() {
        out.push(match k {
            Kind::Partner => ("partner", "partner of (married to)"),
            _ => *first,
        });
        out.extend(second.iter().copied());
    }
    out
}

/// A form value as (kind, whether the two people swap places).
pub fn parse(v: &str) -> Option<(Kind, bool)> {
    DEFS.iter().find_map(|(k, first, second, _)| {
        if first.0 == v {
            Some((*k, false))
        } else if second.is_some_and(|s| s.0 == v) {
            Some((*k, true))
        } else {
            None
        }
    })
}

/// The stored key of "`a` is `kind` of `b`", or None for oneself or an
/// unknown kind.
pub fn key(a: u64, b: u64, kind: u8) -> Option<(u64, u64, u8)> {
    let k = Kind::from_u8(kind)?;
    if a == b {
        return None;
    }
    Some(if k.directed() {
        (a, b, kind)
    } else {
        (a.min(b), a.max(b), kind)
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
    /// Whether this person is stored first (the elder of a directed kind).
    pub first: bool,
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
                first,
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
        assert_eq!(parse("grandchild"), Some((Kind::Grandparent, true)));
        assert_eq!(key(5, 2, Kind::Godparent as u8), Some((5, 2, 12)));
        assert_eq!(key(5, 2, Kind::Cousin as u8), Some((2, 5, 10)));
        assert_eq!(Kind::Teacher.label(false), "student of");
        // Every kind has a number of its own and a form value on each side.
        let kinds: Vec<u8> = (0..=255).filter(|v| Kind::from_u8(*v).is_some()).collect();
        assert_eq!(kinds, (0..=16).collect::<Vec<u8>>());
        let values: Vec<&str> = choices().iter().map(|c| c.0).collect();
        assert_eq!(values.len(), 22);
        assert!(values.iter().all(|v| parse(v).is_some()));
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
