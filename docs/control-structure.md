# Control structure

The STPA (`artifacts/safety.yaml`) and STPA-Sec (`artifacts/security.yaml`)
analyses share this control structure. Names are roles, not hosts.

```
  household member (browser)
        |  requests pages, thumbnails, originals     ^ HTML, images
        v                                            |
  reverse proxy (TLS, base path)  <-- identity provider (OIDC, planned)
        |
        v
  +------------------- lichtbild serve (one process) -------------------+
  |  web handlers  -- read -->  index (redb)  <-- write --  scan thread  |
  |       |                     derived files               (low prio,  |
  |       |  read by item id                                 bounded    |
  |       v                                                  workers)   |
  +-------|----------------------------------------------------|--------+
          v                                                    v
  source roots (read-only)  <------------ list, read ----------+
                                                               |
                         one video keyframe on stdin           v
                   decode-keyframe child (timeout, CPU and memory limits)
                         RGB frame on stdout  --------------->  scan thread
          ^
          |  nightly copy (outside lichtbild) writes
```

Controllers and their control actions:

| Controller | Control action | Controlled process |
|---|---|---|
| scan thread | list, stat, open for reading | source roots |
| scan thread | insert, update, mark missing | index |
| scan thread | write then rename | derived files |
| scan thread | start with one keyframe, kill at the timeout | decode-keyframe child (DD-016) |
| web handlers | query scoped to the viewer | index |
| web handlers | open for reading by item id | source roots, derived files |
| web handlers | open for reading, fixed path or confined to its directory | map files (PMTiles, glyphs, sprites; DD-017) |
| identity provider | issue session | web handlers |
| operator (deployment) | set roots, data directory, limits; read-only mount | lichtbild |

`lichtbild scan` runs the same scan as a one-off command when no server
holds the index (redb allows one process, DD-004).

Feedback: the scan report (found, indexed, skipped by reason) is the
operator's feedback on the scan; the timeline's weak-date marks are the
household's feedback on metadata quality.
