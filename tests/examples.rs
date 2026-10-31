//! `trg --examples` is living documentation, and this is what keeps it alive.
//! It builds the tree `trg::examples::FILES` describes, runs every example's
//! trg command in it, and checks the exit code the example claims. It also
//! fails when an option in `--help` has no example, so a new flag cannot ship
//! without one.

mod helpers;

use std::path::{Path, PathBuf};
use std::process::Command;

use trg::examples::{FILES, SECTIONS};

fn access(day: &str) -> Vec<u8> {
    format!(
        "10.0.0.7 - - [{day}:10:00:01 +0200] \"GET / HTTP/1.1\" 200 5120 \"-\" \"Mozilla/5.0\"\n\
         10.0.0.7 - - [{day}:10:00:02 +0200] \"POST /checkout HTTP/1.1\" 500 128 \"-\" \"Mozilla/5.0\"\n\
         192.168.1.20 - - [{day}:10:01:13 +0200] \"GET /api/orders/4711 HTTP/1.1\" 200 2048 \"-\" \"curl/8.1.2\"\n\
         203.0.113.9 - - [{day}:10:02:40 +0200] \"GET /wp-login.php HTTP/1.1\" 404 0 \"-\" \"sqlmap/1.7\"\n\
         10.0.0.7 - - [{day}:10:03:05 +0200] \"POST /checkout HTTP/1.1\" 200 256 \"-\" \"Mozilla/5.0\"\n\
         192.168.1.20 - - [{day}:10:04:59 +0200] \"GET /api/orders HTTP/1.1\" 503 0 \"-\" \"curl/8.1.2\"\n"
    )
    .into_bytes()
}

fn error(day: &str) -> Vec<u8> {
    format!(
        "[{day} 10:00:02 2026] [php:error] [pid 811] PHP Fatal error:  Allowed memory size exhausted in /srv/shop/checkout.php on line 88\n\
         [{day} 10:04:59 2026] [proxy:error] [pid 812] Timeout waiting for backend 127.0.0.1:9000\n\
         [{day} 10:05:00 2026] [core:notice] [pid 700] AH00052: child pid 811 exit signal Segmentation fault (11)\n"
    )
    .into_bytes()
}

/// Copy a fixture into the tree, keeping only its bytes.
fn put(root: &Path, rel: &str, bytes: &[u8]) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, bytes).unwrap();
}

fn put_fixture(root: &Path, rel: &str, f: helpers::Fixture) {
    put(root, rel, &std::fs::read(&f.path).unwrap());
}

fn build_tree() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();

    let mut rotated = access("22/Aug/2026");
    rotated = {
        use std::io::Write;
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(&rotated).unwrap();
        e.finish().unwrap()
    };
    let core = b"\x7fELF\x02\x01\x01\0\0\0\0\0\0\0\0\0Segmentation fault at 0x0\0\0\n";
    put_fixture(r, "logs/web-2026-08-23.tgz", helpers::tgz("x", &[
        ("access.log", &access("23/Aug/2026")),
        ("access.log.1.gz", &rotated),
        ("error.log", &error("Sun Aug 23")),
        ("core.811", core),
    ]));

    let mut a24 = access("24/Aug/2026");
    a24.extend_from_slice(
        b"198.51.100.4 - - [24/Aug/2026:10:06:00 +0200] \"GET / HTTP/1.1\" 200 5120 \"-\" \"\x1b]2;owned\x07Mozilla/5.0\"\n",
    );
    put_fixture(r, "logs/web-2026-08-24.tar.zst", helpers::tar_zst("x", &[
        ("access.log", &a24),
        ("error.log", &error("Mon Aug 24")),
    ]));
    put_fixture(r, "logs/2026-07/web-2026-07-31.tar.xz", helpers::tar_xz("x", &[
        ("access.log", &access("31/Jul/2026")),
        ("error.log", &error("Fri Jul 31")),
    ]));
    put(r, "logs/app-2026-08-24.zip", &helpers::zip_bytes(&[
        ("app/server.log", b"2026-08-24T10:00:01Z WARN  started with --dry-run\n\
            2026-08-24T10:00:02Z ERROR order=4711 payment declined: card expired\n\
            2026-08-24T10:00:03Z WARN  retrying order=4711 (attempt 2)\n\
            2026-08-24T10:00:09Z INFO  order=4712 paid\n"),
        ("app/worker.log", b"2026-08-24T10:00:04Z INFO  job 47110 done\n\
            2026-08-24T10:00:05Z ERROR job 47111 failed: Timeout talking to smtp\n"),
    ], helpers::ZipMethod::Deflated));
    put_fixture(r, "logs/mail-2026-08-24.tar.bz2", helpers::tar_bz2("x", &[(
        "maillog",
        b"Aug 24 10:00:07 mx1 postfix/smtp[901]: 4F1A2: to=<anna@example.com>, status=bounced (host said: 550 user unknown)\n\
          Aug 24 10:00:08 mx1 postfix/smtp[902]: 4F1A3: to=<ola@example.com>, status=sent\n",
    )]));
    put_fixture(r, "logs/db-2026-08-24.tar.lz4", helpers::tar_lz4("x", &[(
        "slow.log",
        b"# Time: 2026-08-24T10:00:02Z\n\
          # Query_time: 4.210  Lock_time: 0.001 Rows_examined: 912000\n\
          SELECT * FROM orders WHERE note LIKE '%4711%';\n",
    )]));
    put(r, "legacy/web-2019-03-01.tar.Z", &helpers::z_bytes(&helpers::tar_bytes(&[(
        "access.log",
        b"10.0.0.3 - - [01/Mar/2019:09:00:00 +0100] \"GET /cart.php HTTP/1.0\" 200 900\n",
    )])));
    put_fixture(r, "legacy/web-2019-02-28.tgz", helpers::tgz("x", &[
        ("access.log", b"10.0.0.3 - - [28/Feb/2019:09:00:00 +0100] \"GET / HTTP/1.0\" 200 512\n"),
        ("access.log.1.Z", &helpers::z_bytes(
            b"10.0.0.3 - - [27/Feb/2019:09:00:00 +0100] \"GET /cart.php HTTP/1.0\" 200 900\n",
        )),
    ]));
    put(r, "logs/iis-2026-08-24.zip", &helpers::zip_bytes(&[(
        "u_ex260824.log",
        b"#Fields: date time c-ip cs-method cs-uri-stem sc-status time-taken\r\n\
          2026-08-24 10:00:01 10.0.0.7 GET / 200 31\r\n\
          2026-08-24 10:00:02 10.0.0.7 POST /checkout 500 2304\r\n",
    )], helpers::ZipMethod::Deflated));
    put(r, "current/access.log", &access("26/Aug/2026"));
    put_fixture(r, "incoming/download.bin", helpers::tgz("x", &[("access.log", &access("25/Aug/2026"))]));
    let big: Vec<u8> = access("25/Aug/2026").repeat(400);
    put_fixture(r, "broken/web-2026-08-25.tgz", helpers::tgz_truncated("x", &[("access.log", &big)], 0.5));
    d
}

/// Every regular file under `dir`, relative to it.
fn files_under(dir: &Path, base: &Path, out: &mut Vec<String>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            files_under(&p, base, out);
        } else {
            out.push(p.strip_prefix(base).unwrap().to_str().unwrap().to_owned());
        }
    }
}

/// The trg command inside an example: from the first `trg ` to the first
/// unquoted shell operator or newline.
fn trg_part(cmd: &str) -> &str {
    let start = cmd
        .match_indices("trg ")
        .map(|(i, _)| i)
        .find(|&i| i == 0 || cmd[..i].ends_with(|c: char| c.is_whitespace() || c == '('))
        .unwrap_or_else(|| panic!("no trg command in {cmd:?}"));
    let rest = &cmd[start..];
    let (mut single, mut double, mut escaped) = (false, false, false);
    for (i, c) in rest.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if !single => escaped = true,
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            '|' | ';' | '&' | '>' | '<' | ')' | '\n' if !single && !double => {
                return rest[..i].trim_end();
            }
            _ => {}
        }
    }
    rest.trim_end()
}

fn bin_dir() -> PathBuf {
    Path::new(env!("CARGO_BIN_EXE_trg")).parent().unwrap().to_owned()
}

fn sh(root: &Path, line: &str) -> std::process::Output {
    let path = format!("{}:{}", bin_dir().display(), std::env::var("PATH").unwrap_or_default());
    Command::new("sh")
        .arg("-c")
        .arg(line)
        .current_dir(root)
        .env("PATH", path)
        .env_remove("LOGS")
        .output()
        .unwrap()
}

#[test]
fn the_tree_is_the_one_the_examples_describe() {
    let d = build_tree();
    let mut built = Vec::new();
    files_under(d.path(), d.path(), &mut built);
    built.sort();
    let mut described: Vec<String> = FILES.iter().map(|f| f.path.to_owned()).collect();
    described.sort();
    assert_eq!(built, described);

    for f in FILES.iter().filter(|f| !f.members.is_empty()) {
        let o = sh(d.path(), &format!("trg -N --archive-sep '|' -T {}", f.path));
        assert_eq!(o.status.code(), Some(0), "{}: {}", f.path, String::from_utf8_lossy(&o.stderr));
        let listed: Vec<String> = String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|l| l.split_once('|').unwrap().1.to_owned())
            .collect();
        assert_eq!(listed, f.members, "{}: the members the examples name", f.path);
    }
}

#[test]
fn every_example_runs_and_exits_as_it_says() {
    let d = build_tree();
    let mut failures = Vec::new();
    for sec in SECTIONS {
        for ex in sec.examples {
            let run = trg_part(ex.cmd);
            let o = sh(d.path(), run);
            let (out, err) = (String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
            let quiet = run.contains(" -q");
            let problem = if o.status.code() != Some(ex.exit) {
                Some(format!("exit {:?}, the example says {}", o.status.code(), ex.exit))
            } else if ex.exit == 0 && !quiet && out.is_empty() {
                Some("exit 0 with no output".into())
            } else if ex.exit == 2 && err.is_empty() {
                Some("exit 2 with nothing on stderr".into())
            } else if err.contains("unexpected argument") || err.contains("invalid option") {
                Some("a usage error".into())
            } else {
                None
            };
            if let Some(p) = problem {
                failures.push(format!("[{}] {run}\n    {p}\n    stdout: {out}\n    stderr: {err}", sec.title));
            }
        }
    }
    assert!(failures.is_empty(), "{} example(s) wrong:\n{}", failures.len(), failures.join("\n"));
}

/// Every option `--help` lists has at least one example using it, by its short
/// or its long name. A new flag fails here until it gets one.
#[test]
fn every_option_in_help_has_an_example() {
    let help = trg::cli::HELP;
    let options = &help[help.find("MATCHING").unwrap()..help.find("EXIT CODES:").unwrap()];

    let mut used = std::collections::HashSet::new();
    for sec in SECTIONS {
        for ex in sec.examples {
            for tok in trg_part(ex.cmd).split_whitespace().skip(1) {
                if let Some(long) = tok.strip_prefix("--") {
                    used.insert(format!("--{}", long.split('=').next().unwrap()));
                } else if let Some(shorts) = tok.strip_prefix('-') {
                    for c in shorts.chars().take_while(|c| c.is_ascii_alphabetic()) {
                        used.insert(format!("-{c}"));
                    }
                }
            }
        }
    }

    let mut missing = Vec::new();
    for line in options.lines() {
        let t = line.trim_start();
        if !t.starts_with('-') || line.len() - t.len() > 8 {
            continue;
        }
        let names: Vec<&str> = t
            .split(|c: char| c == ',' || c.is_whitespace())
            .take_while(|w| w.is_empty() || w.starts_with('-'))
            .filter(|w| !w.is_empty())
            .collect();
        if names.contains(&"--help") {
            continue;
        }
        if !names.iter().any(|n| used.contains(*n)) {
            missing.push(names.join(", "));
        }
    }
    assert!(missing.is_empty(), "options in --help with no example: {missing:?}");
}

#[test]
fn the_text_fits_a_terminal_and_has_no_control_bytes() {
    let o = Command::new(env!("CARGO_BIN_EXE_trg")).arg("--examples").output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    let s = String::from_utf8(o.stdout).unwrap();
    assert!(s.starts_with(&format!("trg {} ", env!("CARGO_PKG_VERSION"))));
    for line in s.lines() {
        assert!(line.chars().count() <= 80, "over 80 columns: {line:?}");
        assert!(!line.chars().any(|c| c.is_control()), "control character in {line:?}");
    }
    for sec in SECTIONS {
        assert!(s.contains(sec.title));
        for ex in sec.examples {
            assert!(s.contains(ex.cmd.lines().next().unwrap()), "missing {:?}", ex.cmd);
        }
    }
}

/// `--examples` needs no pattern and no path, like `--help`.
#[test]
fn examples_needs_nothing_else_on_the_command_line() {
    let o = Command::new(env!("CARGO_BIN_EXE_trg")).args(["--examples"]).output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert!(o.stderr.is_empty());
}

/// Writes the example tree to `$TRG_EXAMPLES_TREE` and keeps it, to read what
/// an example prints while writing one:
///
///     TRG_EXAMPLES_TREE=/tmp/ex cargo test --test examples -- --ignored
#[test]
#[ignore]
fn write_the_tree_for_a_human() {
    let to = PathBuf::from(std::env::var("TRG_EXAMPLES_TREE").expect("set TRG_EXAMPLES_TREE"));
    let d = build_tree();
    let mut files = Vec::new();
    files_under(d.path(), d.path(), &mut files);
    for f in files {
        let dest = to.join(&f);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::copy(d.path().join(&f), dest).unwrap();
    }
}
