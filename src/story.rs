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
    fn pick(&mut self, pool: &[&'static str]) -> &'static str {
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

// ---- Poems (REQ-STORY-002): rhymed couplets and a haiku ----
//
// Small models cannot rhyme or count syllables, so the rhyme words are
// fixed in the templates and the photo's facts sit inside the lines; a
// haiku line takes a fact only when its syllables make the count.

const DE_V_OPEN: &[&str] = &[
    "Im {month}, als alles schlief, / ein Abenteuer nach uns rief.",
    "Hört zu, ihr Leute, groß und klein, / so muss es einst gewesen sein.",
];
const DE_V_PLACE: &[&str] = &[
    "In {place}, so wird erzählt, / hat der Zauber nie gefehlt.",
    "Nach {place} führte uns der Weg, / über Stock und über Steg.",
];
const DE_V_THINGS: &[&str] = &[
    "Seht nur, {Things} hier und dort, / keiner wollte wieder fort.",
    "{Things} standen still im Licht, / mehr verrät das Foto nicht.",
    "Wer hat die {Things} mitgebracht? / Gewiss die gute Fee der Nacht.",
];
const DE_V_PEOPLE: &[&str] = &[
    "{People} [lachte|lachten] hell und froh, / und das blieb den ganzen Tag so.",
    "{People}, gut gelaunt und heiter, / und die Reise ging dann weiter.",
];
const DE_V_SIGN: &[&str] = &["Ein Schild, auf dem „{words}“ stand, / war das Rätsel in dem Land."];
const DE_V_SAID: &[&str] = &["„{words}“, so klang es laut, / und alle haben hingeschaut."];
const DE_V_PLAIN: &[&str] = &[
    "Ein Bild, so still wie Schnee, / ein Augenblick, juchhe!",
    "Hier ist nichts geschehn, / doch schön war's anzusehn.",
];
const DE_V_NEXT: &[&str] = &["Ein neuer Tag, ein {weekday} gar, / und alles war so wunderbar."];
const DE_V_END: &[&str] = &[
    "Ende gut und alles gut, / und wir ziehn den Zauberhut.",
    "Und ist die Karte endlich voll, / dann war es einfach toll.",
];

const EN_V_OPEN: &[&str] = &[
    "In {month}, when all was still, / adventure called us up the hill.",
    "Gather round, both great and small, / here's the tale to tell you all.",
];
const EN_V_PLACE: &[&str] = &[
    "In {place}, as legends tell, / magic always worked out well.",
    "To {place} the road would wind, / leaving all our cares behind.",
];
const EN_V_THINGS: &[&str] = &[
    "Look, {a_thing}, here it stands, / waiting in enchanted lands.",
    "Who brought {a_thing} today? / Surely fairies, people say.",
];
const EN_V_PEOPLE: &[&str] = &[
    "{People} laughed, so bright and gay, / and stayed that way the livelong day.",
    "{People}, cheerful as could be, / smiled for all the world to see.",
];
const EN_V_SIGN: &[&str] =
    &["A sign that read “{words}” so bold / became a riddle, so we're told."];
const EN_V_SAID: &[&str] =
    &["“{words}”, someone said out loud, / and turned the heads of all the crowd."];
const EN_V_PLAIN: &[&str] = &[
    "A picture quiet as the snow, / a moment's glow, and off we go.",
    "Nothing happened here, it's true, / but what a lovely view.",
];
const EN_V_NEXT: &[&str] =
    &["A brand new day, a {weekday} too, / with skies of gold and skies of blue."];
const EN_V_END: &[&str] = &[
    "All's well that ends well, so they say, / we'll find more magic another day.",
    "And when the memory card is full, / we'll say the trip was wonderful.",
];

/// Haiku lines: (template, syllables of its fixed words). A line fits when
/// those and the facts filled in make 5, 7 or 5.
const DE_HAIKU: [&[(&str, usize)]; 3] = [
    &[
        ("{place} im {month}", 1),
        ("{Things} im Licht", 2),
        ("Morgen in {place}", 3),
        ("Ein Tag voller Licht", 5),
        ("Stille im Bild hier", 5),
    ],
    &[
        ("{People} lachen im Wind", 4),
        ("und überall {Things}", 4),
        ("der Wind erzählt uns davon", 7),
        ("die Zeit bleibt kurz für uns stehn", 7),
    ],
    &[
        ("der {month} vergeht still", 4),
        ("und dann war es gut", 5),
        ("ein Bild bleibt für uns", 5),
    ],
];
const EN_HAIKU: [&[(&str, usize)]; 3] = [
    &[
        ("{place} in {month}", 1),
        ("{a_thing} in light", 2),
        ("morning in {place}", 3),
        ("a day full of light", 5),
        ("stillness in the frame", 5),
    ],
    &[
        ("{People} laugh in the wind", 4),
        ("and everywhere {a_thing}", 4),
        ("the wind tells us all of it", 7),
        ("time stands still for us a while", 7),
    ],
    &[
        ("{month} passes by", 3),
        ("and all was well then", 5),
        ("a picture remains", 5),
    ],
];

/// Syllables of some words, by vowel groups (German diphthongs and long
/// vowels are one group; an English final silent e is dropped). Good
/// enough for names, towns and things; the fixed words are counted by hand.
pub fn syllables(text: &str, lang: Lang) -> usize {
    text.split(|c: char| !c.is_alphabetic())
        .filter(|w| !w.is_empty())
        .map(|w| {
            let w = w.to_lowercase();
            let vowel = |c: char| "aeiouyäöüéèáàíóú".contains(c);
            let mut n = 0;
            let mut prev = false;
            for c in w.chars() {
                let v = vowel(c);
                if v && !prev {
                    n += 1;
                }
                prev = v;
            }
            // An English final e is silent, except in a consonant + "le"
            // ending (bi-cy-cle, but while).
            let cs: Vec<char> = w.chars().collect();
            let consonant_le =
                cs.len() >= 3 && cs.ends_with(&['l', 'e']) && !vowel(cs[cs.len() - 3]);
            if lang == Lang::En && n > 1 && w.ends_with('e') && !consonant_le {
                n -= 1;
            }
            n.max(1)
        })
        .sum()
}

/// A poem: a haiku for the whole, then rhymed couplets picture by picture.
#[derive(Debug, Clone, PartialEq)]
pub struct Poem {
    pub title: String,
    pub haiku: Vec<String>,
    pub opening: Vec<String>,
    pub verses: Vec<(u64, Vec<String>)>,
    pub ending: Vec<String>,
}

fn lines(s: String) -> Vec<String> {
    s.split(" / ").map(str::to_string).collect()
}

/// The haiku: for each line, a template whose count comes out right,
/// with a fact if one fits, else a fixed line.
fn haiku(t: &mut Teller, lang: Lang, f: &Fill) -> Vec<String> {
    let table = match lang {
        Lang::De => DE_HAIKU,
        Lang::En => EN_HAIKU,
    };
    let filled_count = |tpl: &str, fixed: usize| -> Option<(String, usize)> {
        let s = fill(tpl, f);
        // Any slot left empty (no place, no thing) does not fit.
        let slots = [
            ("{place}", f.place.to_string()),
            ("{Things}", fill("{Things}", f)),
            ("{a_thing}", fill("{a_thing}", f)),
            ("{People}", fill("{People}", f)),
            ("{month}", fill("{month}", f)),
        ];
        let mut n = fixed;
        for (k, v) in &slots {
            if tpl.contains(k) {
                if v.trim().is_empty() {
                    return None;
                }
                n += syllables(v, lang);
            }
        }
        Some((s, n))
    };
    let mut out = Vec::new();
    for (row, want) in table.iter().zip([5, 7, 5]) {
        let fits: Vec<&'static str> = row
            .iter()
            .filter(|(tpl, fixed)| filled_count(tpl, *fixed).is_some_and(|(_, n)| n == want))
            .map(|(tpl, _)| *tpl)
            .collect();
        let with_fact: Vec<&'static str> =
            fits.iter().copied().filter(|x| x.contains('{')).collect();
        let pool = if with_fact.is_empty() {
            fits
        } else {
            with_fact
        };
        if pool.is_empty() {
            continue;
        }
        let tpl = t.pick(&pool);
        out.push(capital(&fill(tpl, f)));
    }
    out
}

/// The poem of `moments`; the same seed, the same poem.
pub fn rhyme(moments: &[Moment], lang: Lang, seed: u64) -> Option<Poem> {
    let story = tell(moments, lang, seed)?;
    let chosen = choose(moments);
    let first = chosen.first()?;
    let p = pools(lang);
    let (open, place, things, people, sign, said, plain, next, end) = match lang {
        Lang::De => (
            DE_V_OPEN,
            DE_V_PLACE,
            DE_V_THINGS,
            DE_V_PEOPLE,
            DE_V_SIGN,
            DE_V_SAID,
            DE_V_PLAIN,
            DE_V_NEXT,
            DE_V_END,
        ),
        Lang::En => (
            EN_V_OPEN,
            EN_V_PLACE,
            EN_V_THINGS,
            EN_V_PEOPLE,
            EN_V_SIGN,
            EN_V_SAID,
            EN_V_PLAIN,
            EN_V_NEXT,
            EN_V_END,
        ),
    };
    let mut t = Teller {
        dice: Dice(
            chosen
                .iter()
                .fold(seed ^ 0x5EED, |h, m| h.rotate_left(7) ^ m.id),
        ),
        used: Vec::new(),
    };
    // The haiku takes the first place, thing and people the poem knows.
    let mut whole = (*first).clone();
    whole.place = chosen.iter().find_map(|m| m.place.clone());
    whole.things = chosen
        .iter()
        .find_map(|m| m.things.first().copied())
        .into_iter()
        .collect();
    whole.people = chosen
        .iter()
        .find(|m| !m.people.is_empty())
        .map(|m| m.people.clone())
        .unwrap_or_default();
    let haiku = haiku(&mut t, lang, &at(&p, &whole, ""));
    let opening = lines(fill(t.pick(open), &at(&p, first, "")));
    let mut verses = Vec::new();
    let (mut day, mut at_place): (Option<&str>, Option<&str>) = (None, None);
    for m in &chosen {
        let f = |w| at(&p, m, w);
        let mut v = Vec::new();
        let d = m.taken.get(..10);
        if day.is_some() && d != day {
            v.extend(lines(fill(t.pick(next), &f(""))));
        }
        day = d;
        if m.place.is_some() && m.place.as_deref() != at_place {
            v.extend(lines(fill(t.pick(place), &f(""))));
            at_place = m.place.as_deref();
        }
        let main = if let Some(w) = &m.sign {
            fill(t.pick(sign), &f(w))
        } else if let Some(w) = &m.said {
            fill(t.pick(said), &f(w))
        } else if !m.things.is_empty() {
            fill(t.pick(things), &f(""))
        } else if !m.people.is_empty() {
            fill(t.pick(people), &f(""))
        } else {
            fill(t.pick(plain), &f(""))
        };
        v.extend(lines(main));
        verses.push((m.id, v));
    }
    let ending = lines(fill(t.pick(end), &at(&p, first, "")));
    Some(Poem {
        title: story.title,
        haiku,
        opening,
        verses,
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
    fn poems_rhyme_in_couplets_and_the_haiku_counts_five_seven_five() {
        for lang in [Lang::De, Lang::En] {
            let po = rhyme(&sample(), lang, 4).unwrap();
            assert_eq!(
                po,
                rhyme(&sample(), lang, 4).unwrap(),
                "same seed, same poem"
            );
            assert_eq!(po.haiku.len(), 3, "{lang:?} {:?}", po.haiku);
            if lang == Lang::De {
                let counts: Vec<usize> = po.haiku.iter().map(|l| syllables(l, lang)).collect();
                assert_eq!(counts, vec![5, 7, 5], "{:?}", po.haiku);
            }
            let table = match lang {
                Lang::De => DE_HAIKU,
                Lang::En => EN_HAIKU,
            };
            // The hand counts of the fixed lines agree with the counter.
            for (row, want) in table.iter().zip([5, 7, 5]) {
                for (tpl, fixed) in row.iter() {
                    if !tpl.contains('{') {
                        assert_eq!(*fixed, want, "{tpl}");
                        assert_eq!(syllables(tpl, lang), want, "{tpl}");
                    }
                }
            }
            for v in po.opening.iter().chain(po.ending.iter()) {
                assert!(!v.contains(['{', '}', '[', ']', '|', '/']), "{v}");
            }
            assert_eq!(po.opening.len(), 2);
            assert_eq!(po.verses.len(), 5, "every picture has its verse");
            for (_, v) in &po.verses {
                assert!(v.len() % 2 == 0 && !v.is_empty(), "couplets: {v:?}");
                for l in v {
                    assert!(!l.contains(['{', '}', '[', ']', '|']), "{l}");
                }
            }
            let all = po
                .verses
                .iter()
                .flat_map(|v| v.1.iter())
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            assert!(all.contains("Eis") && all.contains("Bergdorf"), "{all}");
        }
        assert_eq!(syllables("Seestadt", Lang::De), 2);
        assert_eq!(syllables("Katzen", Lang::De), 2);
        assert_eq!(syllables("Eichhörnchen", Lang::De), 3);
        assert_eq!(syllables("bicycle", Lang::En), 3);
        assert_eq!(syllables("cake", Lang::En), 1);
        assert_eq!(syllables("while", Lang::En), 1);
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
