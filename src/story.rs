//! Little made-up stories from a day or a trip (REQ-STORY-001, DD-028):
//! fairy-tale templates filled with what lichtbild knows about the
//! pictures (the day, the town, who is in them, the things and the words
//! seen or said). No language model: every sentence comes from a template
//! below, so nothing is invented beyond the joke, and the same seed always
//! tells the same story.

/// What one picture brings to a story.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Moment {
    pub id: u64,
    /// "YYYY-MM-DDTHH:MM..." as on the timeline.
    pub taken: String,
    /// The nearest town.
    pub place: Option<String>,
    /// Names of the people in it (the viewer knows them, not hidden).
    pub people: Vec<String>,
    /// Kinds of things seen, surest first (`objects::KINDS` index).
    pub things: Vec<u8>,
    /// A short line of text on a sign or page.
    pub sign: Option<String>,
    /// A few words said in a video.
    pub said: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    De,
    En,
}

impl Lang {
    pub fn parse(v: Option<&str>) -> Lang {
        match v {
            Some("en") => Lang::En,
            _ => Lang::De,
        }
    }
}

/// A told story: its title and, picture by picture, what happened.
#[derive(Debug, Clone, PartialEq)]
pub struct Story {
    pub title: String,
    pub opening: String,
    pub chapters: Vec<(u64, String)>,
    pub ending: String,
}

/// At most this many pictures carry a story, so it stays short.
pub const CHAPTERS_MAX: usize = 12;

// Templates. {people} names (or "our heroes"), {Things} a plural kind of
// thing (German) or {a_thing} with its article (English), {place}, {words},
// {weekday}, {month}, {year}. [one|many] picks by how many people act.

const DE_TITLE: &[&str] = &[
    "Die Legende von {place}",
    "Das Abenteuer im {month} {year}",
    "Die wundersame Reise nach {place}",
    "Chroniken eines {weekday}s",
];
const DE_TITLE_NOWHERE: &[&str] = &[
    "Das Abenteuer im {month} {year}",
    "Chroniken eines {weekday}s",
    "Ein Märchen aus dem {month}",
];
const DE_OPEN: &[&str] = &[
    "Es war einmal, an einem {weekday} im {month} {year}, als {people} [beschloss|beschlossen], dass dies kein gewöhnlicher Tag werden würde.",
    "Die alten Chroniken berichten von einem {weekday} im {month} {year}. {People} [ahnte|ahnten] noch nichts.",
    "Vor gar nicht allzu langer Zeit, im {month} {year}, [brach|brachen] {people} zu einem Abenteuer auf.",
    "Als im {month} {year} die Sterne günstig standen, [packte|packten] {people} die Kamera ein.",
];
const DE_PLACE: &[&str] = &[
    "Der Weg führte nach {place}, wo angeblich seit Jahrhunderten niemand pünktlich zu Mittag gegessen hatte.",
    "In {place} angekommen, [wurde|wurden] {people} von einer unsichtbaren Blaskapelle empfangen.",
    "{place} lag friedlich da. Zu friedlich.",
    "Man erzählt sich, in {place} wohne ein Drache, der nur sonntags Feuer spuckt.",
];
const DE_THINGS: &[&str] = &[
    "Plötzlich: {Things}! Überall {Things}.",
    "Niemand weiß genau, wer die {Things} bestellt hatte.",
    "{People} [entdeckte|entdeckten] {Things}, vermutlich verzaubert.",
    "Ein weiser Uhu flüsterte: »Gebt gut acht, die {Things} kommen.« Natürlich hörte niemand auf ihn.",
    "Die {Things} hielten Hof, und alle verneigten sich.",
];
const DE_PEOPLE: &[&str] = &[
    "{People} [lächelte|lächelten], als [wüsste|wüssten] [sie|sie] ein Geheimnis.",
    "Dann geschah es: {people} [schaute|schauten] direkt in die Kamera. Die Kamera schaute zurück.",
    "{People} [war|waren] in Bestform, sagen jedenfalls die Feen.",
];
const DE_SIGN: &[&str] = &[
    "Ein altes Schild verkündete in goldenen Lettern: „{words}“. Die Gelehrten streiten bis heute über die Bedeutung.",
    "An einer Wand stand geschrieben: „{words}“. Ein Zeichen? Ganz sicher.",
    "Eine geheime Botschaft tauchte auf: „{words}“.",
];
const DE_SAID: &[&str] = &[
    "Und dann sprach jemand die legendären Worte: „{words}“",
    "Ein Raunen ging durch die Menge: „{words}“",
];
const DE_PLAIN: &[&str] = &[
    "Was dann geschah, ist nur auf diesem Bild überliefert.",
    "Ein Moment, so still, dass man die Feen kichern hörte.",
    "Hier machte die Geschichte eine kleine Pause und trank einen Kakao.",
    "Die Zeit blieb kurz stehen, um sich das anzusehen.",
];
const DE_NEXT_DAY: &[&str] = &[
    "Am nächsten Morgen ging es weiter.",
    "Ein neuer Tag brach an, ein {weekday}.",
    "Die Sonne ging auf, und das Abenteuer auch.",
];
const DE_END: &[&str] = &[
    "Und wenn sie nicht gestorben sind, dann sortieren sie noch heute ihre Fotos.",
    "Ende. Oder doch nicht? Die nächste Speicherkarte wird es zeigen.",
    "So endete die Geschichte, und die Kamera schlief zufrieden ein.",
];
const DE_HEROES: &str = "unsere Heldinnen und Helden";
const DE_MONTHS: [&str; 12] = [
    "Januar",
    "Februar",
    "März",
    "April",
    "Mai",
    "Juni",
    "Juli",
    "August",
    "September",
    "Oktober",
    "November",
    "Dezember",
];
const DE_DAYS: [&str; 7] = [
    "Montag",
    "Dienstag",
    "Mittwoch",
    "Donnerstag",
    "Freitag",
    "Samstag",
    "Sonntag",
];

const EN_TITLE: &[&str] = &[
    "The Legend of {place}",
    "The {month} {year} Adventure",
    "The Wondrous Journey to {place}",
    "Chronicles of a {weekday}",
];
const EN_TITLE_NOWHERE: &[&str] = &[
    "The {month} {year} Adventure",
    "Chronicles of a {weekday}",
    "A Tale from {month}",
];
const EN_OPEN: &[&str] = &[
    "Once upon a time, on a {weekday} in {month} {year}, {people} decided this would be no ordinary day.",
    "The old chronicles speak of a {weekday} in {month} {year}. {People} suspected nothing.",
    "Not so very long ago, in {month} {year}, {people} set out on an adventure.",
    "When the stars were right in {month} {year}, {people} packed the camera.",
];
const EN_PLACE: &[&str] = &[
    "The road led to {place}, where legend says nobody has eaten lunch on time for centuries.",
    "In {place}, {people} [was|were] greeted by an invisible brass band.",
    "{place} lay peaceful. Too peaceful.",
    "They say a dragon lives in {place}, breathing fire only on Sundays.",
];
const EN_THINGS: &[&str] = &[
    "Suddenly: {a_thing}! Clearly enchanted.",
    "Nobody knows who ordered {a_thing}.",
    "{People} discovered {a_thing}, almost certainly magic.",
    "A wise owl whispered: “Beware of {a_thing}.” Of course nobody listened.",
    "{A_thing} held court, and everyone bowed.",
];
const EN_PEOPLE: &[&str] = &[
    "{People} smiled as if [he or she knew|they knew] a secret.",
    "Then it happened: {people} looked straight into the camera. The camera looked back.",
    "{People} [was|were] in top form, or so the fairies say.",
];
const EN_SIGN: &[&str] = &[
    "An ancient sign proclaimed in golden letters: “{words}”. Scholars still argue about what it means.",
    "Written on a wall: “{words}”. A sign? Surely.",
    "A secret message appeared: “{words}”.",
];
const EN_SAID: &[&str] = &[
    "And then someone spoke the legendary words: “{words}”",
    "A murmur went through the crowd: “{words}”",
];
const EN_PLAIN: &[&str] = &[
    "What happened next survives only in this picture.",
    "A moment so quiet you could hear the fairies giggle.",
    "Here the story took a short break for a hot chocolate.",
    "Time stood still for a moment, to have a look.",
];
const EN_NEXT_DAY: &[&str] = &[
    "The next morning, on it went.",
    "A new day dawned, a {weekday}.",
    "The sun came up, and so did the adventure.",
];
const EN_END: &[&str] = &[
    "And they lived happily ever after, still sorting their photos to this day.",
    "The end. Or is it? The next memory card will tell.",
    "And so the story ended, and the camera fell happily asleep.",
];
const EN_HEROES: &str = "our heroes";
const EN_MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const EN_DAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

struct Pools {
    title: &'static [&'static str],
    title_nowhere: &'static [&'static str],
    open: &'static [&'static str],
    place: &'static [&'static str],
    things: &'static [&'static str],
    people: &'static [&'static str],
    sign: &'static [&'static str],
    said: &'static [&'static str],
    plain: &'static [&'static str],
    next_day: &'static [&'static str],
    end: &'static [&'static str],
    heroes: &'static str,
    and: &'static str,
    months: [&'static str; 12],
    days: [&'static str; 7],
}

fn pools(lang: Lang) -> Pools {
    match lang {
        Lang::De => Pools {
            title: DE_TITLE,
            title_nowhere: DE_TITLE_NOWHERE,
            open: DE_OPEN,
            place: DE_PLACE,
            things: DE_THINGS,
            people: DE_PEOPLE,
            sign: DE_SIGN,
            said: DE_SAID,
            plain: DE_PLAIN,
            next_day: DE_NEXT_DAY,
            end: DE_END,
            heroes: DE_HEROES,
            and: "und",
            months: DE_MONTHS,
            days: DE_DAYS,
        },
        Lang::En => Pools {
            title: EN_TITLE,
            title_nowhere: EN_TITLE_NOWHERE,
            open: EN_OPEN,
            place: EN_PLACE,
            things: EN_THINGS,
            people: EN_PEOPLE,
            sign: EN_SIGN,
            said: EN_SAID,
            plain: EN_PLAIN,
            next_day: EN_NEXT_DAY,
            end: EN_END,
            heroes: EN_HEROES,
            and: "and",
            months: EN_MONTHS,
            days: EN_DAYS,
        },
    }
}

/// A small seeded generator (splitmix64): the same seed, the same story.
struct Dice(u64);

impl Dice {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// Picks from pools without repeating a template until a pool runs out.
struct Teller {
    dice: Dice,
    used: Vec<&'static str>,
}

impl Teller {
    fn pick(&mut self, pool: &'static [&'static str]) -> &'static str {
        let fresh: Vec<&'static str> = pool
            .iter()
            .copied()
            .filter(|t| !self.used.contains(t))
            .collect();
        let from: &[&'static str] = if fresh.is_empty() { pool } else { &fresh };
        let t = from[(self.dice.next() % from.len() as u64) as usize];
        self.used.push(t);
        t
    }
}

/// "Anna", "Anna und Ben", "Anna, Ben und Cleo".
fn names(people: &[String], and: &str) -> String {
    match people {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} {and} {last}", rest.join(", ")),
    }
}

fn capital(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// The weekday (0 = Monday) and month (0-based) of "YYYY-MM-DD...".
fn date_parts(taken: &str) -> Option<(usize, usize, i32)> {
    let d = chrono::NaiveDate::parse_from_str(taken.get(..10)?, "%Y-%m-%d").ok()?;
    use chrono::Datelike;
    Some((
        d.weekday().num_days_from_monday() as usize,
        d.month0() as usize,
        d.year(),
    ))
}

/// What fills one template.
struct Fill<'a> {
    p: &'a Pools,
    people: &'a [String],
    thing: Option<u8>,
    place: &'a str,
    words: &'a str,
    taken: &'a str,
}

/// The fill for one picture's sentence.
fn at<'a>(p: &'a Pools, m: &'a Moment, words: &'a str) -> Fill<'a> {
    Fill {
        p,
        people: &m.people,
        thing: m.things.first().copied(),
        place: m.place.as_deref().unwrap_or(""),
        words,
        taken: &m.taken,
    }
}

fn fill(t: &str, f: &Fill) -> String {
    let who = if f.people.is_empty() {
        f.p.heroes.to_string()
    } else {
        names(f.people, f.p.and)
    };
    let one = f.people.len() == 1;
    let (wd, mo, yr) = date_parts(f.taken).unwrap_or((0, 0, 0));
    let kind = f
        .thing
        .and_then(|k| crate::objects::KINDS.get(k as usize).copied());
    let (things_de, a_thing) = match kind {
        Some((en, _, pl)) => {
            let article = if en.starts_with(['a', 'e', 'i', 'o', 'u']) {
                "an"
            } else {
                "a"
            };
            (pl.to_string(), format!("{article} {en}"))
        }
        None => (String::new(), String::new()),
    };
    let mut s = t
        .replace("{People}", &capital(&who))
        .replace("{people}", &who)
        .replace("{Things}", &things_de)
        .replace("{A_thing}", &capital(&a_thing))
        .replace("{a_thing}", &a_thing)
        .replace("{place}", f.place)
        .replace("{words}", f.words)
        .replace("{weekday}", f.p.days[wd])
        .replace("{month}", f.p.months[mo])
        .replace("{year}", &yr.to_string());
    // [one|many]
    while let Some(a) = s.find('[') {
        let Some(b) = s[a..].find(']').map(|b| a + b) else {
            break;
        };
        let inner = s[a + 1..b].to_string();
        let pick = match inner.split_once('|') {
            Some((sg, pl)) => {
                if one {
                    sg.to_string()
                } else {
                    pl.to_string()
                }
            }
            None => inner,
        };
        s.replace_range(a..=b, &pick);
    }
    s
}

/// How much a picture has to tell.
fn score(m: &Moment) -> usize {
    3 * usize::from(m.sign.is_some() || m.said.is_some())
        + 2 * m.people.len().min(2)
        + usize::from(!m.things.is_empty())
}

/// The pictures that carry the story: by day, the ones with the most to
/// tell, in time order, at most CHAPTERS_MAX in all.
pub fn choose(moments: &[Moment]) -> Vec<&Moment> {
    let mut days: Vec<&str> = moments.iter().filter_map(|m| m.taken.get(..10)).collect();
    days.dedup();
    let per_day = (CHAPTERS_MAX / days.len().max(1)).clamp(1, 4);
    let mut out: Vec<&Moment> = Vec::new();
    for d in &days {
        let mut of: Vec<&Moment> = moments
            .iter()
            .filter(|m| m.taken.get(..10) == Some(*d))
            .collect();
        of.sort_by(|a, b| score(b).cmp(&score(a)).then(a.taken.cmp(&b.taken)));
        of.truncate(per_day);
        out.extend(of);
    }
    out.sort_by(|a, b| a.taken.cmp(&b.taken).then(a.id.cmp(&b.id)));
    out.truncate(CHAPTERS_MAX);
    out
}

/// Tell the story of `moments` (in time order). Another `seed`, another
/// story from the same pictures.
pub fn tell(moments: &[Moment], lang: Lang, seed: u64) -> Option<Story> {
    let chosen = choose(moments);
    let first = chosen.first()?;
    let p = pools(lang);
    let start = chosen.iter().fold(seed, |h, m| h.rotate_left(5) ^ m.id);
    let mut t = Teller {
        dice: Dice(start),
        used: Vec::new(),
    };
    // The cast: everyone in the story, most pictures first.
    let mut cast: Vec<(String, usize)> = Vec::new();
    for m in &chosen {
        for n in &m.people {
            match cast.iter_mut().find(|(c, _)| c == n) {
                Some(c) => c.1 += 1,
                None => cast.push((n.clone(), 1)),
            }
        }
    }
    cast.sort_by_key(|c| std::cmp::Reverse(c.1));
    let cast: Vec<String> = cast.into_iter().take(3).map(|(n, _)| n).collect();
    let home = chosen.iter().find_map(|m| m.place.clone());
    let title = {
        let pool = if home.is_some() {
            p.title
        } else {
            p.title_nowhere
        };
        fill(
            t.pick(pool),
            &Fill {
                p: &p,
                people: &cast,
                thing: None,
                place: home.as_deref().unwrap_or(""),
                words: "",
                taken: &first.taken,
            },
        )
    };
    let opening = fill(
        t.pick(p.open),
        &Fill {
            p: &p,
            people: &cast,
            thing: None,
            place: "",
            words: "",
            taken: &first.taken,
        },
    );
    let mut chapters = Vec::new();
    let (mut day, mut place): (Option<&str>, Option<&str>) = (None, None);
    for m in &chosen {
        let mut parts: Vec<String> = Vec::new();
        let f = |words| at(&p, m, words);
        let d = m.taken.get(..10);
        if day.is_some() && d != day {
            parts.push(fill(t.pick(p.next_day), &f("")));
        }
        day = d;
        if m.place.is_some() && m.place.as_deref() != place {
            parts.push(fill(t.pick(p.place), &f("")));
            place = m.place.as_deref();
        }
        let line = if let Some(w) = &m.sign {
            fill(t.pick(p.sign), &f(w))
        } else if let Some(w) = &m.said {
            fill(t.pick(p.said), &f(w))
        } else if !m.things.is_empty() {
            fill(t.pick(p.things), &f(""))
        } else if !m.people.is_empty() {
            fill(t.pick(p.people), &f(""))
        } else {
            fill(t.pick(p.plain), &f(""))
        };
        parts.push(line);
        // Words told, and there were things too: they get their sentence.
        if (m.sign.is_some() || m.said.is_some()) && !m.things.is_empty() {
            parts.push(fill(t.pick(p.things), &f("")));
        }
        chapters.push((m.id, parts.join(" ")));
    }
    let ending = fill(
        t.pick(p.end),
        &Fill {
            p: &p,
            people: &cast,
            thing: None,
            place: "",
            words: "",
            taken: &first.taken,
        },
    );
    Some(Story {
        title,
        opening,
        chapters,
        ending,
    })
}

/// A sign's text worth quoting: one line of 3 to 32 characters, mostly
/// letters; the surest such line.
pub fn sign_line(lines: &[(String, f32)]) -> Option<String> {
    lines
        .iter()
        .filter(|(t, _)| {
            let t = t.trim();
            let n = t.chars().count();
            let letters = t.chars().filter(|c| c.is_alphabetic()).count();
            (3..=32).contains(&n) && letters * 2 >= n
        })
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(t, _)| unquote(t))
        .filter(|t| t.chars().count() >= 3)
}

/// A line without the quote marks around it (the story adds its own).
fn unquote(t: &str) -> String {
    t.trim()
        .trim_matches(|c: char| "\"'„“”‚‘’«»‹›".contains(c))
        .trim()
        .to_string()
}

/// The first words said in a video: at most about 60 characters, cut at a
/// word, with "…" when cut.
pub fn said_words(text: &str) -> Option<String> {
    let t = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() < 3 {
        return None;
    }
    if t.chars().count() <= 60 {
        return Some(t);
    }
    let mut out = String::new();
    for w in t.split(' ') {
        if out.chars().count() + w.chars().count() + 1 > 60 {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    Some(out + "…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(id: u64, taken: &str) -> Moment {
        Moment {
            id,
            taken: taken.into(),
            ..Moment::default()
        }
    }

    fn sample() -> Vec<Moment> {
        let mut a = m(1, "2025-07-05T10:00");
        a.place = Some("Seestadt".into());
        a.people = vec!["Anna".into()];
        let mut b = m(2, "2025-07-05T12:00");
        b.things = vec![55];
        b.place = Some("Seestadt".into());
        let mut c = m(3, "2025-07-06T09:00");
        c.sign = Some("Eis".into());
        c.place = Some("Bergdorf".into());
        let mut d = m(4, "2025-07-06T15:00");
        d.people = vec!["Anna".into(), "Ben".into()];
        d.said = Some("Schau mal da".into());
        vec![a, b, c, d, m(5, "2025-07-06T16:00")]
    }

    #[test]
    fn the_same_seed_tells_the_same_story_another_seed_another() {
        let s = sample();
        let one = tell(&s, Lang::De, 0).unwrap();
        assert_eq!(one, tell(&s, Lang::De, 0).unwrap());
        let others: Vec<Story> = (1..6).map(|k| tell(&s, Lang::De, k).unwrap()).collect();
        assert!(others.iter().any(|o| o != &one), "seeds change the story");
        assert!(tell(&[], Lang::De, 0).is_none());
    }

    #[test]
    fn what_was_seen_said_and_who_was_there_is_in_the_story() {
        let st = tell(&sample(), Lang::De, 3).unwrap();
        let all = format!(
            "{} {} {} {}",
            st.title,
            st.opening,
            st.chapters
                .iter()
                .map(|c| c.1.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            st.ending
        );
        for want in ["Eis", "Schau mal da", "Anna", "Bergdorf", "Juli"] {
            assert!(all.contains(want), "{want}: {all}");
        }
        // Each picture once, in time order; the empty one too.
        let ids: Vec<u64> = st.chapters.iter().map(|c| c.0).collect();
        assert_eq!(ids, vec![1, 2, 3, 4, 5]);
        let en = tell(&sample(), Lang::En, 3).unwrap();
        assert!(
            en.opening.contains("July")
                || en.title.contains("July")
                || en.title.contains("Seestadt")
        );
    }

    #[test]
    fn every_template_fills_without_leftovers_in_both_languages() {
        for lang in [Lang::De, Lang::En] {
            let p = pools(lang);
            let all = [
                p.title,
                p.title_nowhere,
                p.open,
                p.place,
                p.things,
                p.people,
                p.sign,
                p.said,
                p.plain,
                p.next_day,
                p.end,
            ];
            for people in [
                vec![],
                vec!["Anna".to_string()],
                vec!["A".into(), "B".into()],
            ] {
                for pool in all {
                    for t in pool {
                        let s = fill(
                            t,
                            &Fill {
                                p: &p,
                                people: &people,
                                thing: Some(15),
                                place: "Seestadt",
                                words: "Eis",
                                taken: "2025-07-05T10:00",
                            },
                        );
                        assert!(
                            !s.contains(['{', '}', '[', ']', '|']),
                            "{lang:?} {t} -> {s}"
                        );
                    }
                }
            }
        }
        // One person acts in the singular.
        let p = pools(Lang::De);
        let one = fill(
            "{People} [lächelte|lächelten].",
            &Fill {
                p: &p,
                people: &["Anna".to_string()],
                thing: None,
                place: "",
                words: "",
                taken: "2025-07-05",
            },
        );
        assert_eq!(one, "Anna lächelte.");
        assert_eq!(
            names(&["A".into(), "B".into(), "C".into()], "und"),
            "A, B und C"
        );
    }

    #[test]
    fn long_trips_keep_the_story_short_and_spread_over_the_days() {
        let many: Vec<Moment> = (0..60)
            .map(|i| {
                m(
                    i,
                    &format!("2025-07-{:02}T{:02}:00", 1 + i / 10, 8 + i % 10),
                )
            })
            .collect();
        let c = choose(&many);
        assert_eq!(c.len(), CHAPTERS_MAX);
        let days: std::collections::BTreeSet<&str> =
            c.iter().filter_map(|m| m.taken.get(..10)).collect();
        assert_eq!(days.len(), 6, "every day has its part");
    }

    #[test]
    fn signs_and_words_are_short_and_readable() {
        let lines = vec![
            ("x".to_string(), 0.99),
            ("12345678".to_string(), 0.9),
            ("Eiscafé Venezia".to_string(), 0.8),
            ("Zimmer".to_string(), 0.7),
        ];
        assert_eq!(sign_line(&lines).as_deref(), Some("Eiscafé Venezia"));
        assert_eq!(sign_line(&[]), None);
        assert_eq!(
            sign_line(&[("\"CAFÉ\"".to_string(), 0.9)]).as_deref(),
            Some("CAFÉ")
        );
        let long =
            "eins zwei drei vier fünf sechs sieben acht neun zehn elf zwölf dreizehn vierzehn";
        let w = said_words(long).unwrap();
        assert!(w.ends_with('…') && w.chars().count() <= 61, "{w}");
        assert_eq!(said_words("  hallo   du ").as_deref(), Some("hallo du"));
        assert_eq!(said_words("ja"), None);
    }
}
