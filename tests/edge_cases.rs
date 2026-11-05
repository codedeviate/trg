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

/// Listed nothing because `-g` kept nothing: a 2, as for a search.
#[test]
fn list_members_with_no_member_past_the_globs_is_a_two() {
    let f = helpers::tgz("le.tgz", &[("logs/a.log", b"whatever\n")]);
    let (out, err, c) = run(&["-T", "-g", "*.nomatch", f.path.to_str().unwrap()]);
    assert!(out.is_empty(), "got {out}");
    assert!(err.contains("-g matched no members"), "got {err}");
    assert_eq!(c, Some(2));
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

/// As in rg, `-m 0` allows no matches at all: nothing printed, exit 1.
#[test]
fn max_count_zero_matches_nothing() {
    let f = helpers::tgz("m.tgz", &[("logs/a.log", b"NEEDLE\nNEEDLE\n")]);
    for mode in [&[][..], &["-c"], &["-l"], &["--json"]] {
        let mut args = mode.to_vec();
        args.extend(["-m", "0", "NEEDLE", f.path.to_str().unwrap()]);
        let (out, err, c) = run(&args);
        assert_eq!(out, "", "{mode:?}: {err}");
        assert_eq!(c, Some(1), "{mode:?}: {err}");
    }
}

/// `-v -m N -A k`: after the Nth line, k more lines, matches as matches and
/// the rest as context, then stop. grep-searcher printed every line after the
/// limit as a match, and rg 15.2 does on some inputs, so this is checked
/// against the answer rather than against rg.
#[test]
fn invert_max_count_with_after_context_stops_after_the_context() {
    let body = b"a\nNEEDLE\nb\nc\nNEEDLE\nd\n";
    let f = helpers::tgz("v.tgz", &[("logs/a.log", body)]);
    let plain = helpers::raw("v.log", body);
    let cases: &[(&[&str], &str, &[&str])] = &[
        (&["-m", "1", "-A", "1"], "NEEDLE", &["1:a", "2-NEEDLE"]),
        (&["-m", "1", "-A", "3"], "NEEDLE", &["1:a", "2-NEEDLE", "3:b", "4:c"]),
        (&["-m", "2", "-C", "1"], "NEEDLE$", &["1:a", "2-NEEDLE", "3:b", "4:c"]),
        (&["-m", "3", "-A", "1"], "^NEEDLE", &["1:a", "2-NEEDLE", "3:b", "4:c", "5-NEEDLE"]),
    ];
    let tgz = format!("{}:logs/a.log", f.path.display());
    let log = plain.path.display().to_string();
    for (flags, pat, want) in cases {
        for prefix in [&tgz, &log] {
            let path = prefix.strip_suffix(":logs/a.log").unwrap_or(prefix);
            let mut args = vec!["-v"];
            args.extend(*flags);
            args.extend([*pat, path]);
            let (out, err, c) = run(&args);
            // `prefix:N:line` or `prefix-N-line`, down to `N:line` or `N-line`.
            let got: Vec<&str> = out.lines().map(|l| &l.strip_prefix(prefix.as_str()).unwrap()[1..]).collect();
            assert_eq!(got, *want, "{flags:?} {pat} on {path}: {err}");
            assert_eq!(c, Some(0));
        }
    }
}

/// As rg puts `--` between files when context is on: between members of one
/// archive and between archives, never before the first line, and only for
/// members that printed something.
#[test]
fn context_puts_a_separator_between_members_and_archives() {
    let a = helpers::tgz(
        "a.tgz",
        &[("x.log", b"a\nNEEDLE\n"), ("quiet.log", b"nothing\n"), ("y.log", b"NEEDLE\nb\n")],
    );
    let b = helpers::tgz("b.tgz", &[("z.log", b"NEEDLE\n")]);
    let (pa, pb) = (a.path.to_str().unwrap(), b.path.to_str().unwrap());
    let (out, err, c) = run(&["-j", "1", "-B", "1", "NEEDLE", pa, pb]);
    let want = [
        format!("{pa}:x.log-1-a"),
        format!("{pa}:x.log:2:NEEDLE"),
        "--".into(),
        format!("{pa}:y.log:1:NEEDLE"),
        "--".into(),
        format!("{pb}:z.log:1:NEEDLE"),
    ];
    assert_eq!(out.lines().collect::<Vec<_>>(), want, "{err}");
    assert_eq!(c, Some(0));

    // No context, or a mode that prints no lines: no separator.
    for flags in [&["NEEDLE"][..], &["-c", "-C", "1", "NEEDLE"], &["-l", "-C", "1", "NEEDLE"]] {
        let mut args = flags.to_vec();
        args.extend([pa, pb]);
        let (out, _, _) = run(&args);
        assert!(!out.lines().any(|l| l == "--"), "{flags:?}: {out}");
    }
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

/// A NUL a few KB into a member makes it binary before anything is searched,
/// as rg skips such a file, in every format alike. gzip and xz return short
/// reads, so the searcher's first buffer used to stop before the NUL: the
/// line above it matched, and a `.tgz` was a 0 where the same member in a
/// `.tar.zst` was a 1.
#[test]
fn a_member_binary_in_its_first_buffer_is_skipped_in_every_format() {
    let mut body = b"NEEDLE early\n".to_vec();
    for i in 0..300 {
        body.extend_from_slice(format!("line {i} of plain text\n").as_bytes());
    }
    body.extend_from_slice(b"bin\0ary\nmore text\n");
    let m: &[(&str, &[u8])] = &[("logs/a.log", &body)];
    let fixtures = [
        helpers::tgz("b.tgz", m),
        helpers::tar_xz("b.tar.xz", m),
        helpers::tar_zst("b.tar.zst", m),
        helpers::tar_bz2("b.tar.bz2", m),
        helpers::tar_lz4("b.tar.lz4", m),
        helpers::tar_plain("b.tar", m),
        helpers::zip("b.zip", m),
        helpers::raw("b.log", &body),
    ];
    for f in &fixtures {
        let (out, err, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
        assert_eq!((out.as_str(), c), ("", Some(1)), "{}: {err}", f.path.display());
    }
}
