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
| F28 | REQ-SCAN-002 | missing thumbnails are left out of the operator's problem list | `undecodable_heic_is_counted_by_format_and_listed` |
| F29 | REQ-MEDIA-002 | the viewer shows a broken image for a HEIC without preview | `viewer_explains_an_undecodable_heic` |
| F30 | REQ-AUTH-001 | every route is public | `every_route_needs_a_session_except_health_static_and_login` |
| F31 | REQ-AUTH-002 | a person may see every library | `login_gives_a_session_scoped_to_the_person` |
| F32 | REQ-AUTH-001 | ID token signatures are not checked | `bad_logins_get_no_session` |
| F33 | REQ-AUTH-001 | the target after login may be another site | `next_after_login_stays_in_the_app` |
| F34 | REQ-AUTH-002 | an unknown identity gets every library instead of 403 | `bad_logins_get_no_session` |
| F35 | REQ-AUTH-001 | a callback is accepted without the browser's login binding (login CSRF) | `a_callback_from_another_browser_is_refused` |
| F36 | REQ-AUTH-001 | pending logins are not capped | `pending_logins_are_capped_and_long_next_is_dropped` |
| F37 | REQ-AUTH-002 | a verified email signs in as a person bound to a subject | `email_does_not_override_a_configured_subject` |
| F38 | REQ-AUTH-001 | a cross-site form signs people out | `a_cross_site_logout_is_refused` |
| F39 | REQ-AUTH-001 | a token endpoint off the issuer's host is used | `endpoints_off_the_issuer_host_are_refused` |
| F40 | REQ-AUTH-001 | an encoded `..` in `next` leaves the base path | `next_after_login_stays_in_the_app` |
| F41 | REQ-NAV-001 | the nav switcher is left out of the page header | `nav_switcher_is_in_every_page_header` |
| F42 | REQ-AUTH-001 | a token with a second audience is accepted without `azp` naming this client | `a_trusted_second_audience_signs_in_only_with_azp` |
| F43 | REQ-MEDIA-001 | a clockwise 90° track matrix (iPhone portrait) is not applied to the poster | `videos_are_indexed_with_poster_length_date_and_place` |
| F44 | REQ-MEDIA-001 | the Apple creation date with its offset is ignored for the UTC `mvhd` time | `videos_are_indexed_with_poster_length_date_and_place` |
| F45 | REQ-MEDIA-001 | a failed poster is counted by container instead of by codec | `videos_are_indexed_with_poster_length_date_and_place` |
| F46 | REQ-MEDIA-001 | a video over the pixel limit is decoded anyway | `videos_over_the_pixel_limit_are_skipped` |
| F47 | REQ-MEDIA-001 | a range request returns one byte too few | `videos_play_with_poster_and_seek_by_range` |
| F48 | REQ-MEDIA-001 | the viewer prefetches a whole video that has no poster | `videos_play_with_poster_and_seek_by_range` |
| F49 | REQ-MEDIA-001 | a hanging keyframe decoder is waited for instead of killed | `a_hanging_decoder_is_killed` |
| F50 | REQ-MEDIA-001 | a failing keyframe decoder takes the whole video out of the index | `a_failing_decoder_costs_only_the_poster` |
| F51 | REQ-MEDIA-001 | unchanged videos drop out of the per-codec count on a rescan | `rescan_keeps_videos_and_counts_failed_posters_by_codec` |
| F52 | REQ-MAP-001 | map clusters ignore the viewer's libraries | `clusters_hold_only_the_libraries_the_viewer_may_see` |
| F53 | REQ-MAP-001 | the same, seen through a real login session | `the_map_shows_a_person_only_their_own_positions` |
| F54 | REQ-MAP-001 | glyph and sprite files are read without containment (a symlink out of the directory is served) | `glyphs_and_sprites_are_confined_to_their_directory` |
| F55 | REQ-MAP-001 | the PMTiles file ignores the Range header | `the_pmtiles_file_is_served_with_byte_ranges` |
| F56 | REQ-MAP-001 | the map page loads MapLibre without a PMTiles file instead of the notice | `without_a_map_file_the_page_says_so_and_clusters_still_work` |
| F57 | REQ-MAP-001 | the PMTiles URL is absolute and bypasses the base path | `the_map_works_under_the_base_path` |
| F58 | REQ-MAP-001 | the map page loads pmtiles.js from a CDN | `the_map_loads_nothing_from_another_host` |
| F59 | REQ-MAP-001 | a vendored file is not the pinned upstream file | `vendored_map_assets_match_their_pinned_sha256` |
| F60 | REQ-MEDIA-004 | token loops are never recognised, so a tone runs through every temperature | `token_loops_are_seen` |
| F61 | REQ-SEARCH-001 | search ignores the library asked for and searches all the viewer's libraries | `spoken_words_are_found_in_scope_and_open_the_video_there` |
| F62 | REQ-MEDIA-004 | failed transcriptions are not stored and are retried on every pass | `the_pass_stores_transcripts_once_and_survives_failures` |
| F63 | REQ-MEDIA-004 | a model file with an unknown hash is accepted | `an_unpinned_model_is_refused` |
| F64 | REQ-SEARCH-001 | a spoken hit opens the video at the start instead of at its moment | `spoken_words_are_found_in_scope_and_open_the_video_there` |
| F65 | REQ-MEDIA-004 | silence counts as voiced | `silence_is_not_voiced` |
| F66 | REQ-WEB-001 | pages behind login drop the version footer | `pages_show_the_running_version` |
| F67 | REQ-AUTH-001 | the login notice pages show the version to anyone | `bad_logins_get_no_session` |
| F68 | REQ-DUP-001 | two camera originals of a burst count as the same picture | `copies_are_grouped_and_bursts_are_not` |
| F69 | REQ-DUP-001 | a copy without its camera date is not recognised | `copies_are_grouped_and_bursts_are_not` |
| F70 | REQ-DUP-001 | the shown copy is chosen among all libraries, not the viewer's | `copies_are_grouped_and_bursts_are_not` |
| F71 | REQ-DUP-001 | the timeline shows every copy | `the_timeline_search_and_viewer_show_one_copy` |
| F72 | REQ-DUP-001 | flat pictures get a fingerprint and match each other | `copies_are_grouped_and_bursts_are_not` |
| F73 | REQ-WEB-001 | pages may be cached by the browser across an upgrade | `pages_show_the_running_version` |
| F74 | REQ-AUTH-001 | the session id itself is stored, not its hash | `sessions_survive_a_restart_but_not_a_removal_or_logout` |
| F76 | REQ-AUTH-001 | sign-out leaves the stored session | `sessions_survive_a_restart_but_not_a_removal_or_logout` |
| F77 | REQ-AUTH-002 | a stored session counts for whoever now has its label | `sessions_survive_a_restart_but_not_a_removal_or_logout` |
| F78 | REQ-WEB-006 | the date chooser puts every month's photos under January | `date_scrubber_jumps_to_years_and_months` |
| F79 | REQ-DUP-002 | everyone's hiding is one shared list | `hiding_is_per_person` |
| F80 | REQ-DUP-002 | another site's form can hide items | `hide_and_keep_choose_what_is_shown` |
| F81 | REQ-AUTH-002 | an item in a library one may not see can be hidden | `hiding_is_per_person` |
| F82 | REQ-DUP-003 | the cleanup list shows paths of libraries one was only granted | `only_ones_own_library_is_listed_for_cleanup` |
| F83 | REQ-DUP-002 | a hidden copy is still the one shown | `hide_and_keep_choose_what_is_shown` |
| F84 | REQ-DUP-003 | the cleanup list counts the same picture in another file as a duplicate file | `the_cleanup_list_names_exact_duplicate_files` |
| F85 | REQ-DUP-002 | "back" after hiding may lead to another site | `hide_and_keep_choose_what_is_shown` |
| F86 | REQ-WEB-011 | the app menu loses its link to the status page | `every_page_has_the_app_menu` |
| F87 | REQ-WEB-009 | the status page's refresh gets the whole page, not the list | `the_status_page_shows_scan_and_transcription` |
| F92 | REQ-WEB-009 | the status page shows totals over all libraries to someone who sees only some | `status_totals_only_for_whoever_sees_every_library` |
| F88 | REQ-WEB-010 | the timeline's photo/video filter is inverted | `photos_or_videos_only` |
| F89 | REQ-WEB-010 | search ignores the photo/video filter | `photos_or_videos_only` |
| F90 | REQ-WORDS-001 | common words are not left out of the cloud | `words_come_from_the_viewers_videos_without_common_words` |
| F91 | REQ-WORDS-001 | the cloud counts videos of libraries the viewer may not see | `words_come_from_the_viewers_videos_without_common_words` |
| F93 | REQ-PLACE-002 | an estimate places further pictures (chaining) | `estimates_do_not_chain` |
| F94 | REQ-PLACE-002 | another library's pictures place a picture | `neighbours_in_time_place_a_picture_as_estimated` |
| F95 | REQ-PLACE-002 | another site's form can set a position | `a_picture_placed_by_hand_places_its_neighbours` |
| F96 | REQ-PLACE-002 | someone who only sees a library can place its pictures | `only_the_owner_places_a_picture_by_hand` |
| F97 | REQ-PLACE-002 | a file's time places a picture | `libraries_and_weak_dates_do_not_mix` |
| F98 | REQ-TEXT-001 | texture junk is kept as text | `texture_junk_is_dropped_and_real_lines_kept` |
| F99 | REQ-TEXT-001 | separate sheets are merged into one document | `the_reader_finds_lines_blocks_and_documents` |
| F100 | REQ-TEXT-001 | an unpinned model file is accepted | `no_model_reads_nothing_and_a_wrong_one_is_refused` |
| F101 | REQ-TEXT-001 | a picture already looked at is looked at again | `what_was_seen_is_stored_searched_and_shown` |
| F102 | REQ-TEXT-001 | without a search every line is marked as a hit | `what_was_seen_is_stored_searched_and_shown` |
| F103 | REQ-SEE-001 | unsure detections are kept as things | `whole_picture_boxes_need_a_sure_find` |
| F104 | REQ-SEE-001 | things are searchable in English only | `what_was_seen_is_stored_searched_and_shown` |
| F107 | REQ-SEARCH-002 | suggestions come from libraries the viewer may not see | `the_search_box_suggests_what_the_libraries_hold` |
| F108 | REQ-SEARCH-002 | common words are suggested | `the_search_box_suggests_what_the_libraries_hold` |
| F109 | REQ-SEARCH-002 | a completion that finds nothing with the earlier words is offered | `the_search_box_suggests_what_the_libraries_hold` |
| F110 | REQ-WEB-012 | photos far apart in time join a series | `photos_taken_one_after_another_that_look_alike_are_one_series` |
| F111 | REQ-WEB-012 | photos that do not look alike join a series | `photos_taken_one_after_another_that_look_alike_are_one_series` |
| F106 | REQ-SEE-001 | a whole-picture texture is kept as a thing | `whole_picture_boxes_need_a_sure_find` |
| F105 | REQ-SEE-002 | search by meaning returns every picture, matching or not | `what_was_seen_is_stored_searched_and_shown` |
| F27 | REQ-PLACE-001 | GeoNames lines are split on `\n` only, leaving `\r` from CRLF files | `crlf_line_endings` |

Not covered by a planted fault yet: REQ-IDX-001 (schema migration), REQ-CFG-001
(error messages), REQ-REL-001 (checked by the release workflow itself at the
first tag).

Retired: F75 (a stored session survives the person's removal from the
configuration). Since the stored session is also bound to the identity the
person signed in with (F77), planting F75 alone cannot succeed: two checks
guard the property, and F77 plants into the one that is left.

