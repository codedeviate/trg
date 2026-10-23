mod helpers;

use std::process::Command;

/// Raw bytes: a log line is attacker-controlled, so it can carry anything.
fn run(args: &[&str]) -> (Vec<u8>, Vec<u8>, Option<i32>) {
    let o = Command::new(env!("CARGO_BIN_EXE_trg")).args(args).output().unwrap();
    (o.stdout, o.stderr, o.status.code())
}

fn has(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// What a malicious log line looks like: set the window title, clear the
/// screen, then print something that looks innocent.
const EVIL: &[u8] = b"GET /x NEEDLE \x1b]0;PWNED\x07\x1b[2Jok\n";

#[test]
fn escape_always_makes_control_bytes_visible_in_a_line() {
    let f = helpers::tgz("e.tgz", &[("logs/a.log", EVIL)]);
    let (out, _, c) = run(&["--escape", "always", "NEEDLE", f.path.to_str().unwrap()]);
    assert!(!out.contains(&0x1b), "a raw ESC reached the output: {out:?}");
    assert!(!out.contains(&0x07), "a raw BEL reached the output: {out:?}");
    assert!(has(&out, br"NEEDLE \x1b]0;PWNED\x07\x1b[2Jok"), "got: {}", String::from_utf8_lossy(&out));
    assert_eq!(c, Some(0));
}

#[test]
fn escape_never_passes_the_bytes_through_unchanged() {
    let f = helpers::tgz("e.tgz", &[("logs/a.log", EVIL)]);
    let (out, _, _) = run(&["--escape", "never", "NEEDLE", f.path.to_str().unwrap()]);
    assert!(has(&out, b"\x1b]0;PWNED\x07\x1b[2Jok"), "never must be byte-exact: {out:?}");
}

/// The default when piped (as here) is `auto`, which means off: scripts and
/// `> report.txt` get the same bytes rg would give them.
#[test]
fn piped_output_is_unescaped_by_default() {
    let f = helpers::tgz("e.tgz", &[("logs/a.log", EVIL)]);
    let (out, _, _) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(has(&out, b"\x1b]0;PWNED\x07"), "auto must be off on a pipe: {out:?}");
}

/// The member name comes out of the archive, so it is just as untrusted as
/// the line.
#[test]
fn a_member_name_is_escaped_too() {
    let f = helpers::tgz("n.tgz", &[("logs/a\x1b[31m.log", b"NEEDLE\n")]);
    let (out, _, _) = run(&["--escape", "always", "NEEDLE", f.path.to_str().unwrap()]);
    assert!(!out.contains(&0x1b), "raw ESC from a member name: {out:?}");
    assert!(has(&out, br"logs/a\x1b[31m.log:1:NEEDLE"), "got: {}", String::from_utf8_lossy(&out));
}

#[test]
fn list_members_escapes_member_names() {
    let f = helpers::tgz("n.tgz", &[("logs/a\x1b[31m.log", b"x\n")]);
    let (out, _, _) = run(&["--escape", "always", "-T", f.path.to_str().unwrap()]);
    assert!(!out.contains(&0x1b), "raw ESC in -T output: {out:?}");
    assert!(has(&out, br"logs/a\x1b[31m.log"), "got: {}", String::from_utf8_lossy(&out));
}

/// trg's own colours go through termcolor, not through the text, so they must
/// survive while the log's escapes are neutralised.
#[test]
fn our_own_colours_survive_escaping() {
    let f = helpers::tgz("e.tgz", &[("logs/a.log", EVIL)]);
    let (out, _, _) = run(&["--escape", "always", "--color", "always", "NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains(&0x1b), "colour escapes were lost: {out:?}");
    assert!(has(&out, br"\x1b]0;PWNED"), "the log's escape was not neutralised: {out:?}");
    assert!(!has(&out, b"\x1b]0;"), "a raw OSC from the log survived: {out:?}");
}

/// A carriage return on its own moves the cursor back over the line, which can
/// hide everything before it. As part of a CRLF line ending it is harmless and
/// common, and escaping it would litter every line of a Windows log.
#[test]
fn a_lone_cr_is_escaped_but_crlf_is_kept() {
    let f = helpers::tgz("r.tgz", &[("logs/a.log", b"secret\rNEEDLE cover\r\nNEEDLE dos\r\n")]);
    let (out, _, _) = run(&["--escape", "always", "NEEDLE", f.path.to_str().unwrap()]);
    let s = String::from_utf8_lossy(&out);
    assert!(s.contains(r"secret\x0dNEEDLE cover"), "lone CR not escaped: {s:?}");
    assert!(s.contains("NEEDLE dos\r\n"), "CRLF must stay a real line ending: {s:?}");
}

/// The 8-bit C1 controls, UTF-8 encoded: U+009B is a CSI on terminals that
/// honour C1 in UTF-8 mode.
#[test]
fn utf8_c1_controls_are_escaped() {
    let f = helpers::tgz("c.tgz", &[("logs/a.log", "NEEDLE \u{9b}2J é\n".as_bytes())]);
    let (out, _, _) = run(&["--escape", "always", "NEEDLE", f.path.to_str().unwrap()]);
    let s = String::from_utf8_lossy(&out);
    assert!(s.contains(r"NEEDLE \u{9b}2J é"), "C1 not escaped, or ordinary UTF-8 mangled: {s:?}");
}

#[test]
fn tabs_and_plain_text_are_untouched() {
    let f = helpers::tgz("t.tgz", &[("logs/a.log", b"NEEDLE\tcol2\tcol3\n")]);
    let (out, _, _) = run(&["--escape", "always", "NEEDLE", f.path.to_str().unwrap()]);
    assert!(has(&out, b"NEEDLE\tcol2\tcol3\n"), "got: {out:?}");
}

/// stderr is a terminal too. Diagnostics carry paths and member names.
#[test]
fn diagnostics_on_stderr_are_escaped() {
    let (_, err, c) = run(&["--escape", "always", "x", "/nonexistent/a\x1b[31mb"]);
    assert!(!err.contains(&0x1b), "raw ESC on stderr: {err:?}");
    assert!(has(&err, br"a\x1b[31mb"), "got: {}", String::from_utf8_lossy(&err));
    assert_eq!(c, Some(2));
}

#[test]
fn a_bad_escape_value_is_an_error() {
    let (_, err, c) = run(&["--escape", "sometimes", "x", "f.tgz"]);
    assert_eq!(c, Some(2));
    assert!(has(&err, b"--escape expects auto|always|never"), "got: {}", String::from_utf8_lossy(&err));
}

/// `--json` is often read on a terminal, and often piped to `jq` with
/// `--escape always`. Either way it must stay valid JSON.
#[test]
fn json_output_stays_valid_json_when_escaped() {
    let f = helpers::tgz("j.tgz", &[("logs/a.log", "NEEDLE \u{9b}31m del\x7f esc\x1b[2J\n".as_bytes())]);
    let (out, _, _) = run(&["--json", "--escape", "always", "NEEDLE", f.path.to_str().unwrap()]);
    let s = String::from_utf8_lossy(&out);
    assert!(s.contains(r"\u009b31m del\u007f esc\u001b[2J"), "got: {s}");
    assert!(!s.contains(r"\x7f") && !s.contains(r"\u{"), "non-JSON escape in JSON output: {s}");
    assert!(!out.contains(&0x7f) && !has(&out, b"\xc2\x9b"), "raw DEL/C1 reached the output: {s}");
}
