//! Bugs found by hunting, each with the command that showed it. What they
//! share is the contract: a 0 or a 1 is a claim that everything was read, and
//! the output is what a line-oriented consumer will take it for.

mod helpers;

use std::io::Write;
use std::process::{Command, Stdio};

fn run(args: &[&str]) -> (Vec<u8>, Vec<u8>, Option<i32>) {
    let o = Command::new(env!("CARGO_BIN_EXE_trg")).args(args).stdin(Stdio::null()).output().unwrap();
    (o.stdout, o.stderr, o.status.code())
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// `hit early`, then about 3 MB that does not compress away, so half of the
/// compressed file is well past everything a search that stops at the first
/// match has read.
fn early_hit(prefix: &[u8]) -> Vec<u8> {
    let mut out = prefix.to_vec();
    out.extend_from_slice(b"hit early\n");
    let mut x = 0x2545_f491_4f6c_dd1du64;
    for i in 0..40_000 {
        write!(out, "line {i} ").unwrap();
        for _ in 0..60 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            out.push(b'a' + (x % 10) as u8);
        }
        out.push(b'\n');
    }
    out
}

fn halved(bytes: Vec<u8>) -> Vec<u8> {
    bytes[..bytes.len() / 2].to_vec()
}

// --- 1: a search that stops early still reads the stream to its end -------

/// `trg -q 'hit early' cut.log.gz` was a 0 with nothing on stderr: the match
/// ended the search and the rest was left unread, where the same data in a
/// .tgz was a 2 in every mode.
#[test]
fn an_early_stop_still_finds_a_cut_stream() {
    let raw = early_hit(b"");
    let files: [(&str, Vec<u8>); 5] = [
        ("cut.log.gz", halved(helpers::gzip_bytes(&raw))),
        ("cut.log.xz", halved(helpers::xz_bytes(&raw))),
        ("cut.log.bz2", halved(helpers::bz2_bytes(&raw))),
        ("cut.log.zst", halved(helpers::zstd_plain_bytes(&raw))),
        ("cut.log.lz4", halved(helpers::lz4_bytes(&raw))),
    ];
    for (name, bytes) in files {
        let f = helpers::raw(name, &bytes);
        let p = f.path.to_str().unwrap();
        for mode in [&["-q"][..], &["-l"], &["-m1"], &["-c", "-m1"], &[]] {
            let mut args = mode.to_vec();
            args.extend(["hit early", p]);
            let (_, err, c) = run(&args);
            assert_eq!(c, Some(2), "{name} {mode:?}: {}", text(&err));
            assert!(text(&err).contains(name), "{name} {mode:?}: no error names the file: {}", text(&err));
        }
    }
}

#[test]
fn an_early_stop_still_checks_the_gzip_trailer() {
    let mut bytes = helpers::gzip_bytes(&early_hit(b""));
    let at = bytes.len() - 6;
    bytes[at] ^= 0xff;
    let f = helpers::raw("badcrc.log.gz", &bytes);
    let (_, err, c) = run(&["-q", "hit early", f.path.to_str().unwrap()]);
    assert_eq!(c, Some(2), "{}", text(&err));
    assert!(!err.is_empty());
}

/// The default mode stops too, at the first match after binary data.
#[test]
fn binary_file_matches_still_reads_a_cut_stream() {
    let f = helpers::raw("cut_bin.log.gz", &halved(helpers::gzip_bytes(&early_hit(b"x\0y\n"))));
    let (out, err, c) = run(&["hit early", f.path.to_str().unwrap()]);
    assert_eq!(c, Some(2), "{}", text(&err));
    assert!(text(&out).contains("binary file matches"), "{}", text(&out));
    assert!(!err.is_empty());
}

#[test]
fn an_early_stop_still_reads_a_damaged_zip_member() {
    let mut bytes = helpers::zip_bytes(&[("a.log", &early_hit(b""))], helpers::ZipMethod::Deflated);
    let at = bytes.len() / 2;
    bytes[at] ^= 0xff;
    let f = helpers::raw("flip.zip", &bytes);
    for mode in ["-q", "-l", "-m1"] {
        let (_, err, c) = run(&[mode, "hit early", f.path.to_str().unwrap()]);
        assert_eq!(c, Some(2), "{mode}: {}", text(&err));
        assert!(!err.is_empty(), "{mode}");
    }
}

/// And nothing healthy became a 2.
#[test]
fn an_early_stop_on_a_healthy_file_is_still_a_zero() {
    let raw = early_hit(b"");
    let plain = helpers::raw("whole.log", &raw);
    let gz = helpers::raw("whole.log.gz", &helpers::gzip_bytes(&raw));
    let zip = helpers::zip("whole.zip", &[("a.log", &raw)]);
    let tgz = helpers::tgz_nested_gz("whole.tgz", "a.log.gz", &raw);
    for f in [&plain, &gz, &zip, &tgz] {
        for mode in ["-q", "-l", "-m1"] {
            let (_, err, c) = run(&[mode, "hit early", f.path.to_str().unwrap()]);
            assert_eq!(c, Some(0), "{:?} {mode}: {}", f.path, text(&err));
            assert!(err.is_empty(), "{:?} {mode}: {}", f.path, text(&err));
        }
    }
}

// --- 2: -v -m 0 ---------------------------------------------------------

/// `-m 0` searches nothing, as in rg: no line, no count, exit 1. With `-v`
/// the searcher reported one inverted match before it looked at the limit,
/// and with `-A` every line.
#[test]
fn inverted_max_count_zero_prints_nothing_and_is_a_one() {
    let f = helpers::tgz("s.tgz", &[("s.log", b"a\nb\nc\n")]);
    let p = f.path.to_str().unwrap();
    for extra in [&[][..], &["-A", "1"], &["-c"], &["-q"], &["-l"], &["--json"], &["-C", "2"]] {
        for invert in [&["-v"][..], &[]] {
            let mut args = invert.to_vec();
            args.extend(["-m", "0"]);
            args.extend(extra);
            args.extend(["zzz", p]);
            let (out, err, c) = run(&args);
            assert_eq!(c, Some(1), "{args:?}: {}", text(&err));
            assert!(out.is_empty(), "{args:?}: {}", text(&out));
        }
    }
}

/// Searching nothing is not reading nothing: the bytes are still read, so a
/// cut archive is still a 2.
#[test]
fn max_count_zero_still_reads_the_archive() {
    let raw = early_hit(b"");
    let tgz = helpers::tgz_truncated("cut.tgz", &[("a.log", &raw)], 0.5);
    let gz = helpers::raw("cut.log.gz", &halved(helpers::gzip_bytes(&raw)));
    for f in [&tgz, &gz] {
        for invert in [&["-v"][..], &[]] {
            let mut args = invert.to_vec();
            args.extend(["-m", "0", "zzz", f.path.to_str().unwrap()]);
            let (_, err, c) = run(&args);
            assert_eq!(c, Some(2), "{args:?}: {}", text(&err));
        }
    }
}

// --- 3: separators ------------------------------------------------------

/// rg writes `--` before a file's first *line*; a file that only says
/// `binary file matches` gets none before it, and the file after it gets one.
#[test]
fn no_separator_before_a_lone_binary_notice_from_another_file() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(d.join("1.log"), b"x\nhit\ny\n").unwrap();
    std::fs::write(d.join("4.log"), b"pre\0bin\nhit\nz\n").unwrap();
    std::fs::write(d.join("5.log"), b"u\nhit\nw\n").unwrap();
    let (out, _, c) = run(&["-A1", "hit", d.to_str().unwrap()]);
    let p = d.to_str().unwrap();
    let want = format!(
        "{p}/1.log:2:hit\n{p}/1.log-3-y\n\
         {p}/4.log: binary file matches (found \"\\0\" byte around offset 3)\n\
         --\n{p}/5.log:2:hit\n{p}/5.log-3-w\n"
    );
    assert_eq!(text(&out), want);
    assert_eq!(c, Some(0));
}

/// A file with lines before its notice still gets its `--`.
#[test]
fn a_separator_before_lines_that_end_in_a_binary_notice() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(d.join("1.log"), b"x\nhit\ny\n").unwrap();
    // The NUL is past the searcher's first buffer, or it is seen before any
    // line is printed.
    let mut two = b"hit\nz\n".to_vec();
    two.extend(b"filler\n".repeat(40_000));
    two.extend_from_slice(b"pre\0bin\nhit\n");
    std::fs::write(d.join("2.log"), two).unwrap();
    let (out, _, _) = run(&["-A1", "hit", d.to_str().unwrap()]);
    let p = d.to_str().unwrap();
    let got = text(&out);
    assert!(got.starts_with(&format!("{p}/1.log:2:hit\n{p}/1.log-3-y\n--\n{p}/2.log:1:hit\n")), "{got}");
}

/// With `--crlf` the separator between two archives ends as every other line
/// does, and as the one between two members of one archive already did.
#[test]
fn the_separator_between_archives_follows_crlf() {
    let a = helpers::tgz("a.tgz", &[("a.log", b"hit\r\nx\r\n")]);
    let dir = a.dir.path();
    let b = helpers::tgz("b.tgz", &[("b.log", b"hit\r\ny\r\n"), ("c.log", b"hit\r\nz\r\n")]);
    let bp = dir.join("b.tgz");
    std::fs::copy(&b.path, &bp).unwrap();
    let (out, _, _) = run(&["--crlf", "-A1", "hit", a.path.to_str().unwrap(), bp.to_str().unwrap()]);
    let got = text(&out);
    assert_eq!(got.matches("--\r\n").count(), 2, "{got:?}");
    assert_eq!(got.matches("--\n").count(), 0, "{got:?}");
}

#[test]
fn list_members_has_no_separators() {
    let a = helpers::tgz("a.tgz", &[("a.log", b"x\n")]);
    let b = helpers::tgz("b.tgz", &[("b.log", b"x\n")]);
    let (out, _, c) = run(&["-T", "-A1", a.path.to_str().unwrap(), b.path.to_str().unwrap()]);
    assert_eq!(text(&out).lines().count(), 2, "{}", text(&out));
    assert!(!text(&out).contains("--"), "{}", text(&out));
    assert_eq!(c, Some(0));
}

// --- 4: a newline in a name --------------------------------------------

const FORGED: &str = "nl\nfake.tgz:auth.log:1:Accepted password for root";

/// A member can be named anything, a second line of output included. In a
/// name, a newline is shown as text whether or not a terminal is reading.
#[test]
fn a_newline_in_a_member_name_cannot_forge_a_line() {
    let f = helpers::tgz("n.tgz", &[(FORGED, b"NEEDLE\n")]);
    let p = f.path.to_str().unwrap();
    for (mode, tail) in [(&[][..], ":1:NEEDLE"), (&["-l"], ""), (&["-c"], ":1")] {
        let mut args = mode.to_vec();
        args.extend(["NEEDLE", p]);
        let (out, _, c) = run(&args);
        let want = format!("{p}:nl\\x0afake.tgz:auth.log:1:Accepted password for root{tail}\n");
        assert_eq!(text(&out), want, "{mode:?}");
        assert_eq!(c, Some(0));
    }
    let (out, _, _) = run(&["-T", p]);
    assert_eq!(text(&out), format!("{p}:nl\\x0afake.tgz:auth.log:1:Accepted password for root\n"));
}

#[test]
fn a_carriage_return_in_a_member_name_is_shown_too() {
    let f = helpers::tgz("n.tgz", &[("a\rb.log", b"NEEDLE\n")]);
    let (out, _, _) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(text(&out).ends_with(":a\\x0db.log:1:NEEDLE\n"), "{:?}", text(&out));
}

/// `--escape never` promises the bytes as they are, and keeps the promise.
#[test]
fn escape_never_leaves_a_newline_in_a_name() {
    let f = helpers::tgz("n.tgz", &[(FORGED, b"NEEDLE\n")]);
    let (out, _, _) = run(&["--escape", "never", "NEEDLE", f.path.to_str().unwrap()]);
    assert!(text(&out).contains("nl\nfake.tgz"), "{:?}", text(&out));
}

/// JSON carries the name as a JSON string, already one line.
#[test]
fn json_names_are_left_to_json() {
    let f = helpers::tgz("n.tgz", &[(FORGED, b"NEEDLE\n")]);
    let (out, _, _) = run(&["--json", "NEEDLE", f.path.to_str().unwrap()]);
    let got = text(&out);
    assert!(got.contains(r"nl\nfake.tgz"), "{got}");
    assert!(!got.contains("x0a"), "{got}");
    assert_eq!(got.lines().count(), 4, "{got}");
}

/// The matched line is the log's own, newline and all.
#[test]
fn a_matched_line_keeps_its_newline() {
    let f = helpers::tgz("n.tgz", &[("a.log", b"NEEDLE 1\nNEEDLE 2\n")]);
    let (out, _, _) = run(&["--escape", "always", "NEEDLE", f.path.to_str().unwrap()]);
    assert_eq!(text(&out).lines().count(), 2);
}

/// Diagnostics name members too, one line each.
#[test]
fn a_newline_in_a_name_cannot_forge_a_diagnostic() {
    let gz = helpers::gzip_bytes(&early_hit(b""));
    let name = "bad\ntrg: all clean.gz";
    let f = helpers::tgz("n.tgz", &[(name, &gz[..gz.len() / 2])]);
    let (_, err, c) = run(&["zzz", f.path.to_str().unwrap()]);
    let got = text(&err);
    assert_eq!(c, Some(2));
    assert_eq!(got.lines().count(), 1, "{got:?}");
    assert!(got.contains("bad\\x0atrg: all clean.gz"), "{got:?}");
}

// --- 5: raw C1 bytes and bidi controls ---------------------------------

#[test]
fn a_raw_c1_byte_and_a_bidi_override_are_escaped() {
    let mut line = b"NEEDLE \x9b2J ".to_vec();
    line.extend("\u{202e}txt.exe \u{2066}x\u{2069} \u{41b}\u{e9}\n".as_bytes());
    let f = helpers::tgz("c.tgz", &[("a.log", &line)]);
    let p = f.path.to_str().unwrap();
    let (out, _, _) = run(&["--escape", "always", "-N", "NEEDLE", p]);
    let want = format!("{p}:a.log:NEEDLE \\x9b2J \\u{{202e}}txt.exe \\u{{2066}}x\\u{{2069}} \u{41b}\u{e9}\n");
    assert_eq!(text(&out), want);
    let (out, _, _) = run(&["--escape", "never", "-N", "NEEDLE", p]);
    assert!(out.ends_with(&line), "never must be byte-exact");
}

// --- 6: -g and the form of a member's name ----------------------------

/// A tar with one member whose name is written as given, which the tar
/// crate's own builder refuses for `/x` and `../x`.
fn tgz_named_raw(name: &str, body: &[u8]) -> helpers::Fixture {
    let mut h = tar::Header::new_gnu();
    h.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
    h.set_size(body.len() as u64);
    h.set_mode(0o644);
    h.set_entry_type(tar::EntryType::Regular);
    h.set_cksum();
    let mut b = tar::Builder::new(Vec::new());
    b.append(&h, body).unwrap();
    helpers::raw("named.tgz", &helpers::gzip_bytes(&b.into_inner().unwrap()))
}

/// `tar -P` keeps the leading `/`, and a tar made from a sibling directory
/// starts with `..`: how the archive was made, not where the log is.
#[test]
fn a_glob_matches_a_member_named_with_a_leading_slash_or_dotdot() {
    for name in ["/logs/access.log", "../logs/access.log", "../../logs/access.log", "./../logs/access.log", "logs/access.log"] {
        let f = tgz_named_raw(name, b"NEEDLE\n");
        let p = f.path.to_str().unwrap();
        let (out, err, c) = run(&["-g", "logs/*", "NEEDLE", p]);
        assert_eq!(c, Some(0), "{name}: {}", text(&err));
        assert!(text(&out).contains("NEEDLE"), "{name}");
        let (_, _, c) = run(&["-g", "!logs/*", "NEEDLE", p]);
        assert_eq!(c, Some(2), "{name}: the exclusion must see it as well");
    }
}

/// The name as written still matches, so no glob selects less than it did.
#[test]
fn a_glob_spelled_like_the_member_still_matches() {
    let f = tgz_named_raw("/logs/access.log", b"NEEDLE\n");
    let (_, _, c) = run(&["-g", "/logs/*", "NEEDLE", f.path.to_str().unwrap()]);
    assert_eq!(c, Some(0));
}

// --- 7, 8: closed standard streams -------------------------------------

/// A pipe whose reading end is already closed, as `2>&1 | head -1` leaves
/// stderr once head has gone.
fn dead_pipe() -> Stdio {
    use std::os::fd::FromRawFd;
    let mut fds = [0; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    unsafe {
        libc::close(fds[0]);
        Stdio::from(std::fs::File::from_raw_fd(fds[1]))
    }
}

/// `eprintln!` panics when the write fails, and the release build aborts on a
/// panic: SIGABRT and a crash report in place of the 2.
#[test]
fn a_closed_stderr_pipe_does_not_change_the_exit_code() {
    let good = helpers::tgz("a.tgz", &[("a.log", b"line\n")]);
    let missing = good.dir.path().join("unreadable.tgz");
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_trg"))
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(dead_pipe())
            .status()
            .unwrap()
            .code()
    };
    let (g, m) = (good.path.to_str().unwrap(), missing.to_str().unwrap());
    assert_eq!(run(&["line", g, m]), Some(2), "an unreadable path");
    assert_eq!(run(&["line", g]), Some(0), "nothing to say");
    assert_eq!(run(&["--inflate", "auto", "line", g]), Some(0), "a deprecation note");
    assert_eq!(run(&["--nonsense", "line", g]), Some(2), "a usage error");
    assert_eq!(run(&["-g", "nothing", "line", g]), Some(2), "-g kept nothing");
}

fn run_closing(fd: i32, args: &[&str]) -> (Vec<u8>, Option<i32>) {
    use std::os::unix::process::CommandExt;
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_trg"));
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    unsafe {
        cmd.pre_exec(move || {
            libc::close(fd);
            Ok(())
        });
    }
    let o = cmd.output().unwrap();
    (o.stderr, o.status.code())
}

/// `trg line a.tgz >&-`: the lines went nowhere and the exit code was 0.
#[test]
fn a_closed_stdout_is_a_two() {
    let f = helpers::tgz("a.tgz", &[("a.log", b"line\n")]);
    let (err, c) = run_closing(1, &["line", f.path.to_str().unwrap()]);
    assert_eq!(c, Some(2), "{}", text(&err));
    assert!(text(&err).contains("standard output is closed"), "{}", text(&err));
}

/// `trg hit - <&-` read nothing and said 1.
#[test]
fn a_closed_stdin_named_by_dash_is_a_two() {
    let f = helpers::tgz("a.tgz", &[("a.log", b"line\n")]);
    let (err, c) = run_closing(0, &["line", "-"]);
    assert_eq!(c, Some(2), "{}", text(&err));
    assert!(text(&err).contains("standard input is closed"), "{}", text(&err));
    // Not asked for, not missed.
    let (err, c) = run_closing(0, &["line", f.path.to_str().unwrap()]);
    assert_eq!(c, Some(0), "{}", text(&err));
}

// --- 9: --nice ---------------------------------------------------------

/// Raising priority takes privilege. Without it the request failed in
/// silence; it is still not an error, since everything is read all the same.
#[test]
fn a_nice_value_that_cannot_be_set_is_said_once() {
    if unsafe { libc::geteuid() } == 0 {
        return; // root may raise its priority
    }
    let f = helpers::tgz("a.tgz", &[("a.log", b"line\n")]);
    let (_, err, c) = run(&["--nice", "-5", "line", f.path.to_str().unwrap()]);
    let got = text(&err);
    assert_eq!(c, Some(0), "{got}");
    assert_eq!(got.lines().count(), 1, "{got}");
    assert!(got.contains("--nice -5"), "{got}");
    // Lowering it never fails, and says nothing.
    let (_, err, c) = run(&["--nice", "15", "line", f.path.to_str().unwrap()]);
    assert_eq!((text(&err).as_str(), c), ("", Some(0)));
}

// --- 10: -T with an output mode ------------------------------------------

/// `-T` lists; `-q`, `-c`, `-l` and `--json` were accepted and ignored.
#[test]
fn list_members_refuses_an_output_mode_it_would_ignore() {
    let f = helpers::tgz("a.tgz", &[("a.log", b"line\n")]);
    for mode in ["-q", "-c", "-l", "--json"] {
        let (out, err, c) = run(&["-T", mode, f.path.to_str().unwrap()]);
        assert_eq!(c, Some(2), "{mode}");
        assert!(out.is_empty(), "{mode}: {}", text(&out));
        assert!(text(&err).contains("-T"), "{mode}: {}", text(&err));
    }
    let (_, _, c) = run(&["-T", "-g", "*.log", f.path.to_str().unwrap()]);
    assert_eq!(c, Some(0));
}
