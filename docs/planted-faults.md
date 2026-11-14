# Planted faults

REQ-FAULT-001: every check that verifies a safety or security requirement is
shown to fail when the property it checks is broken. `scripts/planted_faults.py`
plants each fault below (one exact snippet replaced in one file), runs the
check, expects a non-zero exit and restores the file. It fails when a check
passes with its fault in place, when a snippet no longer matches (the list
went stale), or when the planted code does not compile: a test check counts as
caught only when a test ran and reported `test result: FAILED`. CI runs it on every pull request (`planted faults` job).
F11 runs without `nice`, because a test process that inherits nice 19 cannot
tell whether the scan lowered its own priority; run the script from a shell
at nice 0.

| Fault | Requirement | Planted change | Check that must fail |
|---|---|---|---|
| F01 | REQ-SCAN-001 | `source::read_limited` writes an `.xmp` sidecar next to the source | `scan_never_changes_the_source` |
| F02 | REQ-SCAN-001 | the same write | `cargo clippy` (disallowed-methods, DD-006) |
| F03 | REQ-SCAN-002 | hidden files are dropped without a count | `report_reconciles_and_names_every_skip` |
| F04 | REQ-SCAN-002 | one decoded file is never written to the index | `report_reconciles_and_names_every_skip` |
| F05 | REQ-SCAN-003 | unchanged files are reprocessed on every scan | `rescan_is_idempotent_and_tracks_changes` |
| F06 | REQ-SCAN-003 | removed files are not marked missing | `rescan_is_idempotent_and_tracks_changes` |
| F07 | REQ-THUMB-001 | temporary files of a killed run are kept | `interrupted_scan_resumes_to_the_same_index` |
| F08 | REQ-CFG-002 | no overlap check on the lexical data directory path | `data_dir_overlapping_a_root_is_refused` |
| F09 | REQ-CFG-002 | no overlap check on the resolved data directory | `data_dir_overlapping_a_root_is_refused` |
| F10 | REQ-META-001 | dates in file names are ignored | `dates_and_positions_carry_their_source` |
| F11 | REQ-SCAN-004 | the scan keeps nice 0 | `scan_lowers_its_priority` |
| F12 | REQ-SCAN-005 | symlinks leaving the root are followed | `report_reconciles_and_names_every_skip` |
| F13 | REQ-WEB-003 | the Content-Security-Policy allows every source | `every_response_is_same_origin_only` |
| F14 | REQ-WEB-003 | the page loads a stylesheet from an outside host | `every_response_is_same_origin_only` |
| F15 | REQ-WEB-004 | no containment check when resolving a stored path | `files_are_served_by_id_with_containment` |
| F16 | REQ-WEB-002 | `<base href>` ignores the base path | `base_path_prefixes_everything` |
| F17 | REQ-WEB-001 | a continuation page repeats the year and month headings | `timeline_groups_by_day_and_pages_with_htmx` |
| F18 | REQ-WEB-005 | requests for any host are answered | `requests_for_other_hosts_are_refused` |
| F19 | REQ-CFG-002 | a root inside the data directory is not checked (half the condition) | `data_dir_overlapping_a_root_is_refused` |
| F20 | REQ-THUMB-001 | thumbnails that failed for lack of cache space are never retried | `failed_thumbnails_are_retried_and_counted` |
| F21 | REQ-WEB-006 | a jump ignores the chosen year or month | `date_scrubber_jumps_to_years_and_months` |
| F22 | REQ-WEB-007 | the viewer shows a photo from another person's timeline | `viewer_respects_the_timeline_it_was_opened_from` |
| F23 | REQ-MEDIA-002 | HEIC is classified as not supported again | `heic_photos_are_indexed_with_thumbnail_and_preview` |
| F24 | REQ-MEDIA-002 | the viewer shows the HEIC original instead of its preview | `viewer_shows_the_jpeg_preview_for_heic` |
| F25 | REQ-PLACE-001 | the place lookup never finds a town | `viewer_info_panel_shows_date_place_and_camera` |
| F26 | REQ-WEB-008 | the info panel drops the camera and exposure | `viewer_info_panel_shows_date_place_and_camera` |
| F27 | REQ-PLACE-001 | GeoNames lines are split on `\n` only, leaving `\r` from CRLF files | `crlf_line_endings` |

Not covered by a planted fault yet: REQ-IDX-001 (schema migration), REQ-CFG-001
(error messages), REQ-REL-001 (checked by the release workflow itself at the
first tag).
