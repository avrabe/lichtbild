# Planted faults

REQ-FAULT-001: every check that verifies a safety or security requirement is
shown to fail when the property it checks is broken. `scripts/planted_faults.py`
plants each fault below (one exact snippet replaced in one file), runs the
check, expects a non-zero exit and restores the file. It fails when a check
passes with its fault in place, or when a snippet no longer matches (the list
went stale). CI runs it on every pull request (`planted faults` job).
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
| F08 | REQ-CFG-002 | no overlap check before the data directory is created | `data_dir_overlapping_a_root_is_refused` |
| F09 | REQ-CFG-002 | no overlap check on the resolved data directory | `data_dir_overlapping_a_root_is_refused` |
| F10 | REQ-META-001 | dates in file names are ignored | `dates_and_positions_carry_their_source` |
| F11 | REQ-SCAN-004 | the scan keeps nice 0 | `scan_lowers_its_priority` |
| F12 | REQ-SCAN-005 | symlinks leaving the root are followed | `report_reconciles_and_names_every_skip` |
| F13 | REQ-WEB-003 | the Content-Security-Policy allows every source | `every_response_is_same_origin_only` |
| F14 | REQ-WEB-003 | the page loads a stylesheet from an outside host | `every_response_is_same_origin_only` |
| F15 | REQ-WEB-004 | no containment check when resolving a stored path | `files_are_served_by_id_with_containment` |
| F16 | REQ-WEB-002 | `<base href>` ignores the base path | `base_path_prefixes_everything` |
| F17 | REQ-WEB-001 | a continuation page repeats the year and month headings | `timeline_groups_by_day_and_pages_with_htmx` |

Not covered by a planted fault yet: REQ-IDX-001 (schema migration), REQ-CFG-001
(error messages), REQ-REL-001 (checked by the release workflow itself at the
first tag).
