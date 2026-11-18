#!/usr/bin/env python3
"""Plant each fault, run the test that should catch it, expect it to fail.

REQ-FAULT-001: every safety and security check is shown to fail on a
planted fault. Each entry replaces one exact snippet in one file, runs one
check, and restores the file whatever happens. The run fails if a snippet
is not found exactly once (the fault list went stale) or if a check passes
with its fault in place (the check cannot fail).

Usage: scripts/planted_faults.py [fault-id ...]
"""

import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# (id, requirement, file, original, planted, command)
# F11 runs without `nice`: a test process that inherits nice 19 cannot tell
# whether the scan lowered its own priority.
NO_NICE = {"F11"}
FAULTS = [
    ("F01", "REQ-SCAN-001", "src/source.rs",
     "    let f = open(root, rel)?;\n",
     "    let f = open(root, rel)?;\n    std::fs::write(root.join(rel).with_extension(\"xmp\"), b\"\")?;\n",
     ["test", "--test", "scan", "scan_never_changes_the_source"]),
    ("F02", "REQ-SCAN-001", "src/source.rs",
     "    let f = open(root, rel)?;\n",
     "    let f = open(root, rel)?;\n    std::fs::write(root.join(rel).with_extension(\"xmp\"), b\"\")?;\n",
     ["clippy", "--all-targets", "--", "-D", "warnings"]),
    ("F03", "REQ-SCAN-002", "src/scan.rs",
     "                    Class::Skip(s) => skip(&mut rr, &mut problems, s, &rel),\n",
     "                    Class::Skip(Skip::HiddenOrSystem) => {}\n                    Class::Skip(s) => skip(&mut rr, &mut problems, s, &rel),\n",
     ["test", "--test", "scan", "report_reconciles_and_names_every_skip"]),
    ("F04", "REQ-SCAN-002", "src/scan.rs",
     "                    tx_open.upsert(\n",
     "                    if rr.new == 1 { continue; }\n                    tx_open.upsert(\n",
     ["test", "--test", "scan", "report_reconciles_and_names_every_skip"]),
    ("F05", "REQ-SCAN-003", "src/scan.rs",
     "                        if let (true, Some(k)) = (unchanged, k) {\n",
     "                        if let (true, Some(k)) = (unchanged && false, k) {\n",
     ["test", "--test", "scan", "rescan_is_idempotent_and_tracks_changes"]),
    ("F06", "REQ-SCAN-003", "src/scan.rs",
     "                tx_m.set_missing(k.id, true)?;\n",
     "",
     ["test", "--test", "scan", "rescan_is_idempotent_and_tracks_changes"]),
    ("F07", "REQ-THUMB-001", "src/datadir.rs",
     "                std::fs::remove_file(e.path())?;\n",
     "",
     ["test", "--test", "scan", "interrupted_scan_resumes_to_the_same_index"]),
    ("F08", "REQ-CFG-002", "src/config.rs",
     "        check_overlap(&roots, &normalize(&data_dir))?;\n",
     "",
     ["test", "--test", "scan", "data_dir_overlapping_a_root_is_refused"]),
    ("F09", "REQ-CFG-002", "src/config.rs",
     "        check_overlap(&roots, &data_dir)?;\n",
     "",
     ["test", "--test", "scan", "data_dir_overlapping_a_root_is_refused"]),
    ("F10", "REQ-META-001", "src/media.rs",
     "            name_date(rel).map(|d| {\n",
     "            None::<NaiveDateTime>.map(|d| {\n",
     ["test", "--test", "scan", "dates_and_positions_carry_their_source"]),
    ("F11", "REQ-SCAN-004", "src/scan.rs",
     "libc::setpriority(libc::PRIO_PROCESS, 0, 19)",
     "libc::setpriority(libc::PRIO_PROCESS, 0, 0)",
     ["test", "--test", "scan", "scan_lowers_its_priority"]),
    ("F12", "REQ-SCAN-005", "src/source.rs",
     "    if !target.starts_with(root) {\n",
     "    if false {\n",
     ["test", "--test", "scan", "report_reconciles_and_names_every_skip"]),
    ("F13", "REQ-WEB-003", "src/web.rs",
     "        HeaderValue::from_static(CSP),\n",
     "        HeaderValue::from_static(\"default-src *\"),\n",
     ["test", "--test", "web", "every_response_is_same_origin_only"]),
    ("F14", "REQ-WEB-003", "src/web.rs",
     "<script src=\"static/htmx-2.0.4.min.js\" defer></script>\n",
     "<script src=\"static/htmx-2.0.4.min.js\" defer></script>\n<link rel=\"stylesheet\" href=\"https://fonts.example/css\">\n",
     ["test", "--test", "web", "every_response_is_same_origin_only"]),
    ("F15", "REQ-WEB-004", "src/source.rs",
     "    if p.starts_with(root) && p != root {\n",
     "    if true {\n",
     ["test", "--test", "web", "files_are_served_by_id_with_containment"]),
    ("F16", "REQ-WEB-002", "src/web.rs",
     "        base = esc(base),\n",
     "        base = esc(\"\"),\n",
     ["test", "--test", "web", "base_path_prefixes_everything"]),
    ("F17", "REQ-WEB-001", "src/web.rs",
     "    let mut prev = prev_day.map(str::to_string);\n",
     "    let mut prev: Option<String> = prev_day.and(None);\n",
     ["test", "--test", "web", "timeline_groups_by_day_and_pages_with_htmx"]),
    ("F18", "REQ-WEB-005", "src/web.rs",
     "    let ok = authority.is_some_and(",
     "    let ok = true || authority.is_some_and(",
     ["test", "--test", "web", "requests_for_other_hosts_are_refused"]),
    ("F19", "REQ-CFG-002", "src/config.rs",
     "        if data_dir.starts_with(&r.path) || r.path.starts_with(data_dir) {\n",
     "        if data_dir.starts_with(&r.path) || false {\n",
     ["test", "--test", "scan", "data_dir_overlapping_a_root_is_refused"]),
    ("F20", "REQ-THUMB-001", "src/scan.rs",
     "                                    \"decode-error\" => true,\n                                    _ => false,\n",
     "                                    _ => true,\n",
     ["test", "--test", "scan", "failed_thumbnails_are_retried_and_counted"]),
    ("F21", "REQ-WEB-006", "src/web.rs",
     ".or_else(|| from_bound.clone().map(|b| (b, 0)));",
     ".or_else(|| from_bound.clone().map(|_| (String::from(\"\\u{7f}\"), 0)));",
     ["test", "--test", "web", "date_scrubber_jumps_to_years_and_months"]),
    ("F22", "REQ-WEB-007", "src/web.rs",
     "        if !labels.contains(&rec.label) {",
     "        if false {",
     ["test", "--test", "web", "viewer_respects_the_timeline_it_was_opened_from"]),
    ("F23", "REQ-MEDIA-002", "src/media.rs",
     "        \"heic\" | \"heif\" => Class::Media(Format::Heic),",
     "        \"heic\" | \"heif\" => Class::Skip(Skip::NotYetSupported(\"heic\".into())),",
     ["test", "--test", "scan", "heic_photos_are_indexed_with_thumbnail_and_preview"]),
    ("F24", "REQ-MEDIA-002", "src/web.rs",
     "    if rec.preview {\n        format!(\"preview/",
     "    if false {\n        format!(\"preview/",
     ["test", "--test", "web", "viewer_shows_the_jpeg_preview_for_heic"]),
    ("F25", "REQ-PLACE-001", "src/places.rs",
     "        let (i, km) = best?;",
     "        let (i, km) = None::<(u32, f64)>?;",
     ["test", "--test", "web", "viewer_info_panel_shows_date_place_and_camera"]),
    ("F26", "REQ-WEB-008", "src/media.rs",
     "    let mut out = Vec::new();\n    let camera",
     "    let mut out = Vec::new();\n    if !out.is_empty() || true { return out; }\n    let camera",
     ["test", "--test", "web", "viewer_info_panel_shows_date_place_and_camera"]),
    ("F27", "REQ-PLACE-001", "src/places.rs",
     "for line in admin1.unwrap_or(\"\").lines() {",
     "for line in admin1.unwrap_or(\"\").split('\\n') {",
     ["test", "--lib", "crlf_line_endings"]),
    ("F28", "REQ-SCAN-002", "src/scan.rs",
     "            problems.push(problem_line(\"thumbnail\", &reason, &label, rel));",
     "",
     ["test", "--test", "scan", "undecodable_heic_is_counted_by_format_and_listed"]),
    ("F29", "REQ-MEDIA-002", "src/web.rs",
     "    if browser_cannot_show && !rec.preview {",
     "    if false {",
     ["test", "--test", "web", "viewer_explains_an_undecodable_heic"]),
    ("F30", "REQ-AUTH-001", "src/web.rs",
     "let public = rel == \"/healthz\"",
     "let public = true || rel == \"/healthz\"",
     ["test", "--test", "auth", "every_route_needs_a_session_except_health_static_and_login"]),
    ("F31", "REQ-AUTH-002", "src/auth.rs",
     "self.labels.iter().any(|l| l == label)",
     "true || self.labels.iter().any(|l| l == label)",
     ["test", "--test", "auth", "login_gives_a_session_scoped_to_the_person"]),
    ("F32", "REQ-AUTH-001", "src/auth.rs",
     ".set_other_audience_verifier_fn(move |a| trusted.iter().any(|t| t == a.as_str()));",
     ".insecure_disable_signature_check().set_other_audience_verifier_fn(move |a| trusted.iter().any(|t| t == a.as_str()));",
     ["test", "--test", "auth", "bad_logins_get_no_session"]),
    ("F33", "REQ-AUTH-001", "src/auth.rs",
     "        && !n.contains(\"://\")\n",
     "",
     ["test", "--test", "auth", "next_after_login_stays_in_the_app"]),
    ("F34", "REQ-AUTH-002", "src/auth.rs",
     ".ok_or(LoginError::NotAPerson)?;",
     ".unwrap_or(Viewer { labels: self.roots.clone(), name: None });",
     ["test", "--test", "auth", "bad_logins_get_no_session"]),
    ("F35", "REQ-AUTH-001", "src/auth.rs",
     "        if !bound {",
     "        if false && !bound {",
     ["test", "--test", "auth", "a_callback_from_another_browser_is_refused"]),
    ("F36", "REQ-AUTH-001", "src/auth.rs",
     "while p.len() >= PENDING_MAX {",
     "while false && p.len() >= PENDING_MAX {",
     ["test", "--test", "auth", "pending_logins_are_capped_and_long_next_is_dropped"]),
    ("F37", "REQ-AUTH-002", "src/auth.rs",
     "                    p.oidc_subject.is_none()\n                        && p.oidc_email",
     "                    true\n                        && p.oidc_email",
     ["test", "--test", "auth", "email_does_not_override_a_configured_subject"]),
    ("F38", "REQ-AUTH-001", "src/web.rs",
     "if site.is_some_and(|s| s != \"same-origin\" && s != \"none\") {",
     "if false && site.is_some() {",
     ["test", "--test", "auth", "a_cross_site_logout_is_refused"]),
    ("F39", "REQ-AUTH-001", "src/auth.rs",
     "if origin(&u) != want {",
     "if false && origin(&u) != want {",
     ["test", "--test", "auth", "endpoints_off_the_issuer_host_are_refused"]),
    ("F40", "REQ-AUTH-001", "src/auth.rs",
     "&& !n.contains('%')",
     "",
     ["test", "--test", "auth", "next_after_login_stays_in_the_app"]),
    ("F41", "REQ-NAV-001", "src/web.rs",
     "maxlength=\"200\"></form>{nav}</header>",
     "maxlength=\"200\"></form></header>",
     ["test", "--test", "web", "nav_switcher_is_in_every_page_header"]),
    ("F42", "REQ-AUTH-001", "src/auth.rs",
     "None if audiences > 1 => Err(\"several audiences but no authorized party\".into()),",
     "",
     ["test", "--test", "auth", "a_trusted_second_audience_signs_in_only_with_azp"]),
    ("F43", "REQ-MEDIA-001", "src/video.rs",
     "90 => img.rotate90(),",
     "90 => img,",
     ["test", "--test", "video", "videos_are_indexed_with_poster_length_date_and_place"]),
    ("F44", "REQ-MEDIA-001", "src/video.rs",
     "let date = key(\"com.apple.quicktime.creationdate\")",
     "let date = key(\"com.apple.quicktime.creation-date\")",
     ["test", "--test", "video", "videos_are_indexed_with_poster_length_date_and_place"]),
    ("F45", "REQ-MEDIA-001", "src/scan.rs",
     ".map_or(job.format.name(), |v| v.codec.as_str());",
     ".map_or(job.format.name(), |_| job.format.name());",
     ["test", "--test", "video", "videos_are_indexed_with_poster_length_date_and_place"]),
    ("F46", "REQ-MEDIA-001", "src/video.rs",
     "if u64::from(tr.width) * u64::from(tr.height) > max_pixels {",
     "if u64::from(tr.width) * u64::from(tr.height) > max_pixels * 1000 {",
     ["test", "--test", "video", "videos_over_the_pixel_limit_are_skipped"]),
    ("F47", "REQ-MEDIA-001", "src/web.rs",
     "tokio_util_stream(f, first, last - first + 1),",
     "tokio_util_stream(f, first, last - first),",
     ["test", "--test", "video", "videos_play_with_poster_and_seek_by_range"]),
    ("F48", "REQ-MEDIA-001", "src/web.rs",
     "if r.video.is_none() || r.preview {",
     "if true {",
     ["test", "--test", "video", "videos_play_with_poster_and_seek_by_range"]),
    ("F49", "REQ-MEDIA-001", "src/video.rs",
     "Ok(None) if Instant::now() < deadline =>",
     "Ok(None) =>",
     ["test", "--lib", "a_hanging_decoder_is_killed"]),
    ("F50", "REQ-MEDIA-001", "src/video.rs",
     "let rgb = run_child(exe, job, max_pixels, CHILD_TIMEOUT).map_err(|_| bad(codec))?;",
     "let rgb = run_child(exe, job, max_pixels, CHILD_TIMEOUT).expect(\"decoded\");",
     ["test", "--test", "video", "a_failing_decoder_costs_only_the_poster"]),
    ("F51", "REQ-MEDIA-001", "src/scan.rs",
     "if let Some(c) = &k.codec {",
     "if let Some(c) = k.codec.as_ref().filter(|_| false) {",
     ["test", "--test", "video", "rescan_keeps_videos_and_counts_failed_posters_by_codec"]),
    ("F52", "REQ-MAP-001", "src/map.rs",
     "        if !labels.iter().any(|l| l.as_str() == &*p.label) {\n",
     "        if false && !labels.iter().any(|l| l.as_str() == &*p.label) {\n",
     ["test", "--test", "map", "clusters_hold_only_the_libraries_the_viewer_may_see"]),
    ("F53", "REQ-MAP-001", "src/map.rs",
     "        if !labels.iter().any(|l| l.as_str() == &*p.label) {\n",
     "        if false && !labels.iter().any(|l| l.as_str() == &*p.label) {\n",
     ["test", "--test", "auth", "the_map_shows_a_person_only_their_own_positions"]),
    ("F54", "REQ-MAP-001", "src/map.rs",
     "        crate::source::read_limited(&dir, &rel, ASSET_MAX_BYTES)\n",
     "        std::fs::read(dir.join(&rel))\n",
     ["test", "--test", "map", "glyphs_and_sprites_are_confined_to_their_directory"]),
    ("F55", "REQ-MAP-001", "src/map.rs",
     "    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());\n",
     "    let range: Option<&str> = headers.get(header::RANGE).and(None);\n",
     ["test", "--test", "map", "the_pmtiles_file_is_served_with_byte_ranges"]),
    ("F56", "REQ-MAP-001", "src/map.rs",
     "    if st.map.pmtiles.is_none() {\n",
     "    if false {\n",
     ["test", "--test", "map", "without_a_map_file_the_page_says_so_and_clusters_still_work"]),
    ("F57", "REQ-MAP-001", "src/map.rs",
     "\" data-pmtiles=\\\"map/tiles.pmtiles\\\"",
     "\" data-pmtiles=\\\"/map/tiles.pmtiles\\\"",
     ["test", "--test", "map", "the_map_works_under_the_base_path"]),
    ("F58", "REQ-MAP-001", "src/map.rs",
     "<script src=\\\"static/{pm}\\\" defer></script>",
     "<script src=\\\"https://unpkg.com/pmtiles@4.5.0/dist/{pm}\\\" defer></script>",
     ["test", "--test", "map", "the_map_loads_nothing_from_another_host"]),
    ("F59", "REQ-MAP-001", "src/map.rs",
     "include_str!(\"../assets/protomaps-basemaps-5.7.2/basemaps.js\")",
     "include_str!(\"../assets/pmtiles-4.5.0/pmtiles.js\")",
     ["test", "--test", "map", "vendored_map_assets_match_their_pinned_sha256"]),
    ("F60", "REQ-MEDIA-004", "src/stt/whisper.rs",
     "tail.chunks(p).all(|c| c == &tail[..p])",
     "tail.chunks(p).all(|c| c == &tail[..p]) && p > 8",
     ["test", "--lib", "token_loops_are_seen"]),
    ("F61", "REQ-SEARCH-001", "src/web.rs",
     "None => \"all\".to_string(),\n    };\n    let Some(labels) = scope(&viewer, &who) else {",
     "None => \"all\".to_string(),\n    };\n    let Some(labels) = Some(viewer.labels.clone()) else {",
     ["test", "--test", "transcribe", "spoken_words_are_found_in_scope_and_open_the_video_there"]),
    ("F62", "REQ-MEDIA-004", "src/stt/mod.rs",
     "idx.set_transcript(&rec.hash, &tr)?;",
     "if tr.state == \"ok\" { idx.set_transcript(&rec.hash, &tr)?; }",
     ["test", "--test", "transcribe", "the_pass_stores_transcripts_once_and_survives_failures"]),
    ("F63", "REQ-MEDIA-004", "src/stt/mod.rs",
     ".find(|(_, sum)| *sum == h)",
     ".find(|_| !h.is_empty())",
     ["test", "--test", "transcribe", "an_unpinned_model_is_refused"]),
    ("F64", "REQ-SEARCH-001", "src/web.rs",
     "Some(t) if rec.video.is_some() =>",
     "Some(t) if false && rec.video.is_some() =>",
     ["test", "--test", "transcribe", "spoken_words_are_found_in_scope_and_open_the_video_there"]),
    ("F65", "REQ-MEDIA-004", "src/stt/mod.rs",
     "let threshold = (floor + 12.0).max(-45.0);",
     "let threshold = f32::NEG_INFINITY.max(floor - 1000.0);",
     ["test", "--lib", "silence_is_not_voiced"]),
    ("F66", "REQ-WEB-001", "src/web.rs",
     "        \"\",\n        &version_footer(),",
     "        \"\",\n        \"\",",
     ["test", "--test", "web", "pages_show_the_running_version"]),
    ("F67", "REQ-AUTH-001", "src/web.rs",
     "        page(&st.base_path, title, &body, &st.nav_html, \"\", \"\"),",
     "        page_for(st, title, &body),",
     ["test", "--test", "auth", "bad_logins_get_no_session"]),
    ("F73", "REQ-WEB-001", "src/web.rs",
     "if html && !h.contains_key(header::CACHE_CONTROL) {",
     "if false && html && !h.contains_key(header::CACHE_CONTROL) {",
     ["test", "--test", "web", "pages_show_the_running_version"]),
]


def cargo(fid):
    exe = shutil.which("cargo")
    if not exe:
        sys.exit("cargo not found")
    if os.getpriority(os.PRIO_PROCESS, 0) != 0 and fid in NO_NICE:
        sys.exit(f"{fid} needs a process at nice 0; this one runs at nice {os.getpriority(os.PRIO_PROCESS, 0)}")
    # Plain nice, not macOS background QoS: taskpolicy -b throttled builds
    # to a crawl even on an idle machine.
    nice = [] if fid in NO_NICE else ["nice", "-n", "19"]
    return nice + [exe]


def main():
    wanted = set(sys.argv[1:])
    env = dict(os.environ, CARGO_BUILD_JOBS=os.environ.get("CARGO_BUILD_JOBS", "2"))
    failures = []
    for fid, req, rel, orig, planted, args in FAULTS:
        if wanted and fid not in wanted:
            continue
        path = ROOT / rel
        text = path.read_text()
        if text.count(orig) != 1:
            failures.append(f"{fid} {req}: snippet found {text.count(orig)} times in {rel} (stale fault)")
            print(f"{fid} STALE", flush=True)
            continue
        try:
            path.write_text(text.replace(orig, planted))
            r = subprocess.run(cargo(fid) + args,
                               cwd=ROOT, env=env, capture_output=True, text=True)
        finally:
            path.write_text(text)
        out = r.stdout + r.stderr
        compiled = "could not compile" not in out and "error[E" not in out
        if args[0] == "test":
            # Caught means a test ran and failed, not that the plant broke the build.
            caught = r.returncode != 0 and compiled and "test result: FAILED" in out
        else:
            caught = r.returncode != 0
        if args[0] == "test" and not compiled:
            failures.append(f"{fid} {req}: the planted code does not compile, so it proves nothing")
            print(f"{fid} {req:<14} DOES NOT COMPILE", flush=True)
            continue
        print(f"{fid} {req:<14} {'caught' if caught else 'NOT CAUGHT'}  ({' '.join(args)}; exit {r.returncode})",
              flush=True)
        if not caught:
            failures.append(f"{fid} {req}: check passed with the fault planted")
    if failures:
        print("\n".join(failures), file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
