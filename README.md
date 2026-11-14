# lichtbild

A self-hosted photo and video library for a household, as one Rust binary.

lichtbild indexes photo folders read-only, in place, without copying or moving them. It serves:

- a timeline per person, or merged;
- a world map of where photos were taken;
- later, face recognition and similar search.

Everything runs on the home server. The map tiles come from a local file, and no photo, place or face leaves the house.

Status: just started. See [docs/BRIEF.md](docs/BRIEF.md) for what it should become.
