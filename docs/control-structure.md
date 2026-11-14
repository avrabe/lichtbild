# Control structure

The STPA (`artifacts/safety.yaml`) and STPA-Sec (`artifacts/security.yaml`)
analyses share this control structure. Names are roles, not hosts.

```
  household member (browser)
        |  requests pages, thumbnails, originals     ^ HTML, images, video
        v                                            |
  reverse proxy (TLS, base path)  <-- identity provider (OIDC)
        |
        v
  lichtbild serve  ------ reads ------>  index (SQLite)  <--- writes --- lichtbild scan
        |                                derived files                    |  low priority,
        |  reads by item id only         (thumbnails)                     |  bounded workers
        v                                                                 v
  source roots (read-only)  <---------------- lists, reads ---------------+
        ^
        |  nightly copy (outside lichtbild) writes
```

Controllers and their control actions:

| Controller | Control action | Controlled process |
|---|---|---|
| lichtbild scan | list, stat, open for reading | source roots |
| lichtbild scan | insert, update, mark missing | index |
| lichtbild scan | write then rename | derived files |
| lichtbild serve | query scoped to the viewer | index |
| lichtbild serve | open for reading by item id | source roots, derived files |
| identity provider | issue session | lichtbild serve |
| operator (deployment) | set roots, data directory, limits; read-only mount | lichtbild |

Feedback: the scan report (found, indexed, skipped by reason) is the
operator's feedback on the scan; the timeline's weak-date marks are the
household's feedback on metadata quality.
