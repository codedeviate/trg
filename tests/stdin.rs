//! `-` reads standard input, as in rg: `zcat x.gz | trg ERR -`, or a whole
//! archive piped in.

mod helpers;

use helpers::run_trg_stdin as run;

#[test]
fn dash_searches_standard_input_named_stdin() {
    let (out, err, c) = run(&["ERR", "-"], b"ok\nan ERR here\n");
    assert_eq!(out, "<stdin>:2:an ERR here\n");
    assert!(err.is_empty(), "{err}");
    assert_eq!(c, Some(0));
}

/// The case that broke: a pipe has no offset, and the check for a log
/// truncated mid-read asked it for one, so every clean pipe was a 2.
#[test]
fn a_pipe_with_no_match_is_a_clean_one() {
    for path in ["-", "/dev/stdin"] {
        let (out, err, c) = run(&["ERR", path], b"ok\nfine\n");
        assert!(out.is_empty(), "{path}: {out}");
        assert!(err.is_empty(), "{path}: {err}");
        assert_eq!(c, Some(1), "{path}");
    }
}

#[test]
fn an_archive_piped_in_is_searched_member_by_member() {
    let tgz = helpers::gzip_bytes(&helpers::tar_bytes(&[("logs/a.log", b"x\nERR one\n")]));
    let (out, err, c) = run(&["ERR", "-"], &tgz);
    assert_eq!(out, "<stdin>:logs/a.log:2:ERR one\n");
    assert!(err.is_empty(), "{err}");
    assert_eq!(c, Some(0));
}

/// Standard input has no name for `-g` to judge; naming it by `-` is the
/// choice, as naming a file is.
#[test]
fn glob_does_not_filter_standard_input() {
    let (out, _, c) = run(&["-g", "*.log", "ERR", "-"], b"ERR\n");
    assert_eq!(out, "<stdin>:1:ERR\n");
    assert_eq!(c, Some(0));
}

#[test]
fn a_cut_stream_on_standard_input_is_a_two() {
    let gz = helpers::gzip_bytes(&b"ERR not this\n".repeat(1000));
    let (_, err, c) = run(&["NOPE", "-"], &gz[..gz.len() / 2]);
    assert!(err.contains("<stdin>"), "names the input: {err}");
    assert_eq!(c, Some(2));
}

#[test]
fn dash_twice_reads_standard_input_once() {
    let (out, _, c) = run(&["ERR", "-", "-"], b"ERR\n");
    assert_eq!(out, "<stdin>:1:ERR\n");
    assert_eq!(c, Some(0));
}
