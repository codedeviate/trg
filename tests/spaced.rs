//! `--spaced`: an empty line after every printed line, never after `--`.

mod helpers;

use helpers::{Fixture, run_trg, tgz};

const LOG: &[u8] = b"one\nhit 1\ntwo\nthree\nfour\nhit 2\nfive";

fn fixture() -> Fixture {
    tgz("a.tgz", &[("a.log", LOG)])
}

fn path(f: &Fixture) -> String {
    f.path.to_str().unwrap().to_string()
}

/// The archive path as trg prints it, then the expected text with `{}` for it.
fn expect(f: &Fixture, template: &str) -> String {
    template.replace("{}", &path(f))
}

const CONTEXT: &str =
    "{}:a.log:2:hit 1\n\n{}:a.log-3-two\n\n--\n{}:a.log:6:hit 2\n\n{}:a.log-7-five\n\n";

fn strip_sgr(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' && it.peek() == Some(&'[') {
            for d in it.by_ref() {
                if d == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[test]
fn every_matching_line_is_followed_by_an_empty_line() {
    let f = fixture();
    let (out, _, code) = run_trg(&["--spaced", "hit", &path(&f)]);
    assert_eq!(out, expect(&f, "{}:a.log:2:hit 1\n\n{}:a.log:6:hit 2\n\n"));
    assert_eq!(code, Some(0));
}

#[test]
fn context_lines_are_spaced_and_the_separator_is_not() {
    let f = fixture();
    let (out, _, code) = run_trg(&["--spaced", "-A", "1", "hit", &path(&f)]);
    assert_eq!(out, expect(&f, CONTEXT));
    assert_eq!(code, Some(0));
}

#[test]
fn without_the_flag_nothing_changes() {
    let f = fixture();
    let (out, _, code) = run_trg(&["hit", &path(&f)]);
    assert_eq!(out, expect(&f, "{}:a.log:2:hit 1\n{}:a.log:6:hit 2\n"));
    assert_eq!(code, Some(0));
}

#[test]
fn crlf_gaps_are_crlf() {
    // A log written with CRLF line endings: the gap copies the row's own
    // terminator, `--` is written with it too, and the unterminated last line
    // gets one.
    let f = tgz(
        "a.tgz",
        &[(
            "a.log",
            &LOG.iter()
                .flat_map(|&b| {
                    if b == b'\n' {
                        vec![b'\r', b'\n']
                    } else {
                        vec![b]
                    }
                })
                .collect::<Vec<u8>>(),
        )],
    );
    let (out, _, code) = run_trg(&["--spaced", "--crlf", "-A", "1", "hit", &path(&f)]);
    assert_eq!(out, expect(&f, &CONTEXT.replace('\n', "\r\n")));
    assert_eq!(code, Some(0));
}

#[test]
fn lf_rows_keep_lf_gaps_under_crlf() {
    // --crlf changes how lines are matched, not the terminator of a row that
    // ends in a bare \n; its gap is a bare \n too.
    let f = fixture();
    let (out, _, _) = run_trg(&["--spaced", "--crlf", "hit 1", &path(&f)]);
    assert_eq!(out, expect(&f, "{}:a.log:2:hit 1\n\n"));
}

#[test]
fn colour_does_not_move_the_gaps() {
    let f = fixture();
    let (out, _, _) = run_trg(&["--spaced", "--color", "always", "-A", "1", "hit", &path(&f)]);
    assert!(out.contains('\x1b'), "expected colour: {out:?}");
    assert_eq!(strip_sgr(&out), expect(&f, CONTEXT));
}

#[test]
fn archives_in_parallel_keep_their_gaps_and_the_separator_between_them_none() {
    let a = fixture();
    let b = fixture();
    let c = fixture();
    let (pa, pb, pc) = (path(&a), path(&b), path(&c));
    let run = |j: &str| run_trg(&["--spaced", "-A", "1", "-j", j, "hit", &pa, &pb, &pc]).0;
    let par = run("4");
    assert_eq!(par, run("1"));
    assert!(par.contains("\n\n--\n"), "{par:?}");
    assert!(!par.contains("--\n\n"), "{par:?}");
}

#[test]
fn only_matching_parts_are_spaced_one_by_one() {
    let f = fixture();
    let (out, _, _) = run_trg(&["--spaced", "-o", "hit [0-9]", &path(&f)]);
    assert_eq!(out, expect(&f, "{}:a.log:2:hit 1\n\n{}:a.log:6:hit 2\n\n"));
}

#[test]
fn modes_without_lines_refuse_it() {
    let f = fixture();
    let p = path(&f);
    for flag in ["-c", "-l", "-q", "--json"] {
        let (_, err, code) = run_trg(&["--spaced", flag, "hit", &p]);
        assert_eq!(code, Some(2), "{flag}");
        assert!(
            err.contains("--spaced") && err.contains(flag),
            "{flag}: {err}"
        );
    }
    let (_, err, code) = run_trg(&["--spaced", "-T", &p]);
    assert_eq!(code, Some(2));
    assert!(err.contains("--spaced") && err.contains("-T"), "{err}");
}
