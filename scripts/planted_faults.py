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
     "                    Class::Skip(s) => skip(&mut rr, s),\n",
     "                    Class::Skip(Skip::HiddenOrSystem) => {}\n                    Class::Skip(s) => skip(&mut rr, s),\n",
     ["test", "--test", "scan", "report_reconciles_and_names_every_skip"]),
    ("F04", "REQ-SCAN-002", "src/scan.rs",
     "                    tx_open.upsert(&root.label,",
     "                    if rr.new == 1 { continue; }\n                    tx_open.upsert(&root.label,",
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
     "        check_overlap(&roots, &lexical)?;\n",
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
]


def cargo(fid):
    exe = shutil.which("cargo")
    if not exe:
        sys.exit("cargo not found")
    if os.getpriority(os.PRIO_PROCESS, 0) != 0 and fid in NO_NICE:
        sys.exit(f"{fid} needs a process at nice 0; this one runs at nice {os.getpriority(os.PRIO_PROCESS, 0)}")
    nice = [] if fid in NO_NICE else ["nice", "-n", "19"]
    if sys.platform == "darwin" and shutil.which("taskpolicy"):
        return nice + ["taskpolicy", "-b", exe]  # background QoS, not a nice value
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
            r = subprocess.run(cargo(fid) + args + (["--quiet"] if args[0] == "test" else []),
                               cwd=ROOT, env=env, capture_output=True, text=True)
        finally:
            path.write_text(text)
        caught = r.returncode != 0
        print(f"{fid} {req:<14} {'caught' if caught else 'NOT CAUGHT'}  ({' '.join(args)}; exit {r.returncode})",
              flush=True)
        if not caught:
            failures.append(f"{fid} {req}: check passed with the fault planted")
    if failures:
        print("\n".join(failures), file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
