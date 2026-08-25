//! The cases that are individually small and collectively the whole product:
//! empty archives, binary members, hardlinks, globs, and every output mode.

mod helpers;

use helpers::run_trg as run;

#[test]
fn an_empty_archive_is_a_clean_no_match() {
    let f = helpers::tgz("e.tgz", &[]);
    let (out, _, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.is_empty());
    assert_eq!(c, Some(1), "an empty archive read successfully is 1, not 2");
}

#[test]
fn a_binary_member_is_skipped_but_dash_a_searches_it() {
    let mut bin = b"NEEDLE".to_vec();
    bin.extend_from_slice(&[0u8, 1, 2, 3, 0, 0]);
    bin.push(b'\n');
    let f = helpers::tgz("b.tgz", &[("logs/blob.bin", &bin)]);
    let p = f.path.to_str().unwrap();

    let (out, _, _) = run(&["NEEDLE", p]);
    assert!(!out.contains("logs/blob.bin:1:"), "binary member should be skipped: {out}");

    let (out_a, _, c) = run(&["-a", "NEEDLE", p]);
    assert!(out_a.contains("logs/blob.bin"), "-a must force it: {out_a}");
    assert_eq!(c, Some(0));
}

#[test]
fn hardlink_members_are_silently_skipped() {
    let f = helpers::tgz_with_hardlink("h.tgz");
    let (out, err, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("logs/real.log"));
    assert!(!out.contains("logs/linked.log"), "hardlink must not be reported: {out}");
    assert!(err.is_empty(), "a skipped hardlink is not an error: {err}");
    assert_eq!(c, Some(0));
}

#[test]
fn member_globs_narrow_the_search() {
    let f = helpers::tgz(
        "g.tgz",
        &[("logs/x.access.log", b"NEEDLE in access\n"), ("logs/x.error.log", b"NEEDLE in error\n")],
    );
    let (out, _, _) = run(&["-g", "*.access.log", "NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("in access"));
    assert!(!out.contains("in error"), "glob did not filter: {out}");
}

#[test]
fn list_members_respects_globs_and_searches_nothing() {
    let f = helpers::tgz(
        "l.tgz",
        &[("logs/x.access.log", b"whatever\n"), ("logs/x.error.log", b"whatever\n")],
    );
    let (out, _, _) =
        run(&["-T", "-g", "*.access.log", f.path.to_str().unwrap()]);
    assert!(out.contains("logs/x.access.log"), "got {out}");
    assert!(!out.contains("logs/x.error.log"));
    assert!(!out.contains(":1:"), "-T must not print match lines: {out}");
}

#[test]
fn list_members_with_no_members_is_a_clean_one() {
    let f = helpers::tgz("le.tgz", &[("logs/a.log", b"whatever\n")]);
    let (out, err, c) = run(&["-T", "-g", "*.nomatch", f.path.to_str().unwrap()]);
    assert!(out.is_empty(), "got {out}");
    assert!(err.is_empty(), "got {err}");
    assert_eq!(c, Some(1));
}

/// `-T` answers a question about the tar headers, so it needs no pattern —
/// but the positional rule used to hand `archive.tgz` to the pattern slot,
/// leaving nothing to walk. The command exited 1 having printed nothing, which
/// is indistinguishable from an archive with no members.
#[test]
fn list_members_needs_no_pattern() {
    let f = helpers::tgz(
        "t.tgz",
        &[("logs/x.access.log", b"whatever\n"), ("logs/x.error.log", b"whatever\n")],
    );
    let (out, err, c) = run(&["-T", f.path.to_str().unwrap()]);
    assert!(out.contains("logs/x.access.log"), "got {out} / {err}");
    assert!(out.contains("logs/x.error.log"), "got {out}");
    assert_eq!(c, Some(0), "listed members is a 0, stderr={err}");
}

/// `-c` was the only mode that went completely silent on a member that matched
/// and then hit binary data: `SummarySink` squashes its count to zero, so
/// stdout stayed empty while the exit code stayed 0. A script doing
/// `n=$(trg -c PAT a.tgz); ((n>0))` reads that as "clean".
///
/// stdout must not change — it is rg-compatible and the count really is
/// unknown — so the correction is a stderr note. The exit code does not move
/// either: everything was read.
#[test]
fn count_mode_says_on_stderr_when_binary_data_suppressed_the_count() {
    // The match has to be reported *before* the binary byte is seen, which
    // means they must land in different buffer fills: grep-searcher's default
    // buffer is 8 KiB, so the NUL goes well past that.
    let mut body = b"NEEDLE here\n".to_vec();
    for i in 0..4000 {
        body.extend_from_slice(format!("filler line {i} padding padding padding\n").as_bytes());
    }
    body.extend_from_slice(b"trailing\x00\x00binary\n");
    let f = helpers::tgz("cb.tgz", &[("logs/big.log", &body)]);
    let p = f.path.to_str().unwrap();

    let (out, err, c) = run(&["-c", "NEEDLE", p]);
    assert!(out.is_empty(), "stdout stays rg-compatible and empty, got {out}");
    assert_eq!(c, Some(0), "the archive was read in full and it matched");
    assert!(
        err.contains("logs/big.log") && err.contains("count suppressed"),
        "-c must not go silent about a suppressed count, stderr={err}"
    );

    // The other modes were never silent and must stay unchanged.
    let (l_out, l_err, _) = run(&["-l", "NEEDLE", p]);
    assert!(l_out.contains("logs/big.log"), "got {l_out}");
    assert!(l_err.is_empty(), "-l has nothing to warn about, got {l_err}");
    let (_, q_err, q_c) = run(&["-q", "NEEDLE", p]);
    assert_eq!(q_c, Some(0));
    assert!(q_err.is_empty(), "-q prints nothing, including notes: {q_err}");
}

#[test]
fn count_and_files_with_matches_modes() {
    let f = helpers::tgz("c.tgz", &[("logs/a.log", b"NEEDLE\nNEEDLE\nno\n")]);
    let p = f.path.to_str().unwrap();

    let (c_out, _, code) = run(&["-c", "NEEDLE", p]);
    assert!(c_out.trim_end().ends_with(":2"), "expected a count of 2, got {c_out}");
    assert_eq!(code, Some(0));

    let (l_out, _, code) = run(&["-l", "NEEDLE", p]);
    assert!(l_out.contains("logs/a.log"));
    assert!(!l_out.contains(":1:"), "-l must not print lines: {l_out}");
    assert_eq!(code, Some(0));
}

#[test]
fn count_and_files_with_matches_print_nothing_when_nothing_matches() {
    let f = helpers::tgz("c0.tgz", &[("logs/a.log", b"nothing\n")]);
    let p = f.path.to_str().unwrap();
    for flag in ["-c", "-l"] {
        let (out, err, code) = run(&[flag, "NEEDLE", p]);
        assert!(out.is_empty(), "{flag} printed {out} for a member with no matches");
        assert!(err.is_empty(), "{flag}: {err}");
        assert_eq!(code, Some(1), "{flag}");
    }
}

/// `-c` counts per member, not per archive: a day's tgz holds one file per
/// vhost and an aggregate would hide which vhost is on fire.
#[test]
fn count_is_reported_per_member() {
    let f = helpers::tgz(
        "cm.tgz",
        &[("logs/a.log", b"NEEDLE\nNEEDLE\n"), ("logs/b.log", b"NEEDLE\n")],
    );
    let (out, _, _) = run(&["-c", "NEEDLE", f.path.to_str().unwrap()]);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "expected one count line per member, got {out}");
    assert!(lines[0].ends_with("logs/a.log:2"), "got {out}");
    assert!(lines[1].ends_with("logs/b.log:1"), "got {out}");
}

#[test]
fn context_flags_produce_context_lines() {
    let f = helpers::tgz("x.tgz", &[("logs/a.log", b"one\ntwo\nNEEDLE\nfour\nfive\n")]);
    let (out, _, _) = run(&["-C", "1", "NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("two"), "missing before-context: {out}");
    assert!(out.contains("four"), "missing after-context: {out}");
}

#[test]
fn max_count_stops_per_member() {
    let f = helpers::tgz("m.tgz", &[("logs/a.log", b"NEEDLE\nNEEDLE\nNEEDLE\n")]);
    let (out, _, _) = run(&["-m", "2", "NEEDLE", f.path.to_str().unwrap()]);
    assert_eq!(out.lines().count(), 2, "expected exactly 2 lines, got: {out}");
}

#[test]
fn archive_sep_changes_the_separator() {
    let f = helpers::tgz("s.tgz", &[("logs/a.log", b"NEEDLE\n")]);
    let (out, _, _) = run(&["--archive-sep", "!", "NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("!logs/a.log:1:"), "got {out}");
}

#[test]
fn json_mode_emits_json_lines() {
    let f = helpers::tgz("j.tgz", &[("logs/a.log", b"NEEDLE\n")]);
    let (out, _, c) = run(&["--json", "NEEDLE", f.path.to_str().unwrap()]);
    assert!(
        out.lines().any(|l| l.contains("\"type\":\"match\"")),
        "expected a JSON Lines match record, got {out}"
    );
    assert!(out.contains("logs/a.log"), "got {out}");
    assert_eq!(c, Some(0));
}

/// `--color` is documented, so it has to do something. Stdout here is a pipe,
/// never a tty, so `auto` must stay colourless and `always` must not.
#[test]
fn color_always_colours_and_a_pipe_does_not() {
    let f = helpers::tgz("col.tgz", &[("logs/a.log", b"NEEDLE\n")]);
    let p = f.path.to_str().unwrap();

    let (plain, _, _) = run(&["NEEDLE", p]);
    assert!(!plain.contains('\x1b'), "a pipe must not get escapes: {plain:?}");

    let (auto, _, _) = run(&["--color", "auto", "NEEDLE", p]);
    assert!(!auto.contains('\x1b'), "auto on a pipe must not colour: {auto:?}");

    let (always, _, _) = run(&["--color", "always", "NEEDLE", p]);
    assert!(always.contains('\x1b'), "--color=always produced no escapes: {always:?}");

    let (never, _, _) = run(&["--color", "never", "NEEDLE", p]);
    assert!(!never.contains('\x1b'), "{never:?}");
}
