//! The project's central hard requirement: **`trg` must never report "no
//! matches" for data it failed to read.**
//!
//! - `0` — at least one match, everything read.
//! - `1` — no matches, and everything was read successfully.
//! - `2` — anything went unread.
//!
//! Exit 1 is a positive claim that the logs are clean, and during an incident
//! someone acts on it. Every test here defends one path that could otherwise
//! produce a 1 without having read everything.

mod helpers;

use helpers::run_trg as run;

#[test]
fn match_found_is_zero() {
    let f = helpers::tgz("a.tgz", &[("logs/a.log", b"NEEDLE\n")]);
    assert_eq!(run(&["NEEDLE", f.path.to_str().unwrap()]).2, Some(0));
}

#[test]
fn no_match_with_everything_read_is_one() {
    let f = helpers::tgz("a.tgz", &[("logs/a.log", b"nothing here\n")]);
    assert_eq!(run(&["NEEDLE", f.path.to_str().unwrap()]).2, Some(1));
}

#[test]
fn a_missing_file_is_two_not_one() {
    assert_eq!(run(&["NEEDLE", "/no/such/archive.tgz"]).2, Some(2));
}

#[test]
fn a_truncated_archive_is_two_not_one() {
    let big = vec![b'x'; 400_000];
    let f = helpers::tgz_truncated("t.tgz", &[("logs/a.log", &big)], 0.5);
    let c = run(&["NEEDLE-not-present", f.path.to_str().unwrap()]).2;
    assert_eq!(c, Some(2), "an unreadable archive must never look like a clean 1");
}

#[test]
fn a_truncated_archive_still_prints_earlier_matches() {
    let big = vec![b'x'; 400_000];
    let f = helpers::tgz_truncated(
        "t2.tgz",
        &[("logs/first.log", b"NEEDLE early\n"), ("logs/second.log", &big)],
        0.55,
    );
    let (out, err, code) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("NEEDLE early"), "partial data beats none: {out}");
    assert_eq!(code, Some(2));
    assert!(!err.is_empty(), "must diagnose on stderr");
}

#[test]
fn one_bad_archive_does_not_abort_the_others() {
    let good = helpers::tgz("good.tgz", &[("logs/a.log", b"NEEDLE good\n")]);
    let (out, _, code) = run(&["NEEDLE", "/no/such.tgz", good.path.to_str().unwrap()]);
    assert!(out.contains("NEEDLE good"));
    assert_eq!(code, Some(2));
}

#[test]
fn quiet_reports_only_through_the_exit_code() {
    let f = helpers::tgz("a.tgz", &[("logs/a.log", b"NEEDLE\n")]);
    let (out, _, code) = run(&["-q", "NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.is_empty(), "-q must print nothing, got {out}");
    assert_eq!(code, Some(0));
}

#[test]
fn quiet_with_no_match_is_one() {
    let f = helpers::tgz("a.tgz", &[("logs/a.log", b"nothing\n")]);
    let (out, _, code) = run(&["-q", "NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.is_empty());
    assert_eq!(code, Some(1));
}

/// The reason `matched` must not be derived from "did we write any bytes":
/// every one of these modes decouples output from matching.
#[test]
fn silent_and_summary_modes_still_report_unread_data_as_two() {
    let big = vec![b'x'; 400_000];
    let f = helpers::tgz_truncated("qt.tgz", &[("logs/a.log", &big)], 0.5);
    let p = f.path.to_str().unwrap();
    for mode in [&["-q"][..], &["-c"][..], &["-l"][..], &["-T"][..], &["--json"][..]] {
        let mut args: Vec<&str> = mode.to_vec();
        args.extend_from_slice(&["NEEDLE", p]);
        let (_, err, code) = run(&args);
        assert_eq!(code, Some(2), "{mode:?} on a truncated archive must be 2, stderr={err}");
    }
}

/// The dangerous direction for the summary modes: they short-circuit a member
/// the moment it matches, so a matching archive that is *also* truncated could
/// plausibly stop before noticing and report a confident 0.
#[test]
fn a_match_does_not_hide_unread_data_in_the_silent_modes() {
    let big = vec![b'x'; 400_000];
    let f = helpers::tgz_truncated(
        "qm.tgz",
        &[("logs/first.log", b"NEEDLE early\n"), ("logs/second.log", &big)],
        0.55,
    );
    let p = f.path.to_str().unwrap();
    for mode in [&["-q"][..], &["-c"][..], &["-l"][..], &["--json"][..], &[][..]] {
        let mut args: Vec<&str> = mode.to_vec();
        args.extend_from_slice(&["NEEDLE", p]);
        let (_, err, code) = run(&args);
        assert_eq!(code, Some(2), "{mode:?} matched but did not read it all; stderr={err}");
        assert!(!err.is_empty(), "{mode:?} must diagnose on stderr");
    }
}

/// `members_searched == 0` is NOT a partial-run signal: an archive whose
/// members are all filtered out by `-g` was read completely and found nothing.
#[test]
fn an_archive_fully_filtered_out_by_globs_is_a_clean_one() {
    let f = helpers::tgz(
        "gf.tgz",
        &[("logs/a.log", b"NEEDLE\n"), ("logs/b.log", b"NEEDLE\n")],
    );
    let (out, err, code) = run(&["-g", "*.nomatch", "NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.is_empty(), "nothing matched the glob: {out}");
    assert!(err.is_empty(), "a filtered-out archive is not an error: {err}");
    assert_eq!(code, Some(1), "seen=2 searched=0 errors=[] is a clean 1, not a 2");
}

/// A usage error means **nothing was read**. Reporting that as 1 would be
/// claiming clean logs on the strength of a typo. `main` returning
/// `anyhow::Result` gives Rust's default exit of 1, so this is a whole class
/// of defect with a single cause.
#[test]
fn usage_errors_exit_two_not_one() {
    let cases: &[&[&str]] = &[
        &["--nonsense", "pat", "f.tgz"],
        &[],
        &["--color", "nonsense", "pat"],
        &["--color=nonsense", "pat"],
        &["--inflate", "sideways", "pat"],
        &["--archive-sep", "::", "pat"],
        &["-j", "abc", "pat"],
        &["--inflate-budget", "abc", "pat"],
        &["--inflate-budget", "5X", "pat"],
        &["--inflate-budget", "-1", "pat"],
        &["--inflate-budget", "", "pat"],
    ];
    for c in cases {
        let (out, err, code) = run(c);
        assert_eq!(code, Some(2), "{c:?} must exit 2; stdout={out} stderr={err}");
        assert!(!err.is_empty(), "{c:?} must diagnose on stderr");
    }
}

#[test]
fn an_unparsable_pattern_exits_two() {
    let f = helpers::tgz("a.tgz", &[("logs/a.log", b"NEEDLE\n")]);
    let (_, err, code) = run(&["-e", "[", f.path.to_str().unwrap()]);
    assert_eq!(code, Some(2), "a bad regex read nothing, so it cannot be a 1");
    assert!(!err.is_empty());
}

#[test]
fn an_invalid_glob_exits_two() {
    let f = helpers::tgz("a.tgz", &[("logs/a.log", b"NEEDLE\n")]);
    let (_, err, code) = run(&["-g", "[", "NEEDLE", f.path.to_str().unwrap()]);
    assert_eq!(code, Some(2));
    assert!(!err.is_empty());
}

#[test]
fn help_and_version_still_exit_zero() {
    assert_eq!(run(&["--help"]).2, Some(0));
    assert_eq!(run(&["--version"]).2, Some(0));
}
