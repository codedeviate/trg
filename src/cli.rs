//! Argument parsing. rg-compatible flag names wherever the job is the same;
//! `-g` deliberately means *member* filter, an approved divergence.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::{print, search};

pub const HELP: &str = "\
trg — search inside .tgz log archives

USAGE:
    trg [OPTIONS] PATTERN PATH...
    trg [OPTIONS] -e PATTERN... PATH...
    trg [OPTIONS] -T PATH...

At least one PATH is required. trg never searches the current directory by
default: a silent walk of $PWD is not a safe guess for a tool whose exit 1
is a claim that the logs are clean.

MATCHING (same meaning as ripgrep):
    -e, --regexp PAT      add a pattern; repeatable
    -i, --ignore-case     case-insensitive
    -S, --smart-case      case-insensitive unless the pattern has uppercase
    -w, --word-regexp     match whole words only
    -F, --fixed-strings   treat patterns as literals
    -v, --invert-match    show non-matching lines
    -a, --text            search members that look binary
    -m, --max-count NUM   stop after NUM matches per member
        --crlf            treat CRLF as the line terminator

OUTPUT (same meaning as ripgrep):
    -n, --line-number     show line numbers (default)
    -N, --no-line-number  hide line numbers
    -A, --after NUM       lines of trailing context
    -B, --before NUM      lines of leading context
    -C, --context NUM     lines of context on both sides
    -o, --only-matching   print only the matching part
    -c, --count           print a count per member
    -l, --files-with-matches
                          print member paths only
    -q, --quiet           print nothing; exit code only
        --json            JSON Lines output
        --color WHEN      auto | always | never (auto: only on a tty)
                          If several output modes are given, the most
                          suppressive wins: -q, then -l, then -c, then --json.

ARCHIVES:
    -g, --glob PAT        filter MEMBERS inside archives; repeatable
    -T, --list-members    list member paths without searching. Takes no
                          PATTERN at all: every positional is a PATH, so
                          `trg -T *.tgz` lists all of them
        --archive-sep C   separator in archive:member:line (default ':')
        --no-sort         allow output in completion order

RESOURCES:
    -j, --jobs NUM        archives searched concurrently (default 1)
        --turbo           -j <ncpus> --nice 0
        --nice NUM        scheduling priority (default 10)
        --no-nice         do not lower priority
        --no-drop-cache   keep archive pages in the page cache
        --inflate MODE    auto | stream | buffer (default auto)
        --inflate-budget SIZE
                          whole-buffer inflate ceiling (default 64M)
        --load-limit N    clamp jobs if 1-min load average exceeds N

    -h, --help            this message
    -V, --version         version

EXIT CODES:
    0  matches found, everything read
    1  no matches, everything read
    2  something went unread — never trust a 1 you did not get

NOTES (documented, and surprising the first time):
    -T lists every member, including ones a search skips as binary.
    Two directories symlinking the same file search it once (identity dedup).
    An intact archive behind a corrupt gzip trailer is a 0; truncation is
      still caught and is a 2.
    --turbo's ordered output is bounded by neither --inflate-budget nor the
      spill cap; peak RSS grows with the match volume held in order.
    On a broken pipe (`| head`) trg stops early and exits 0, leaving later
      archives unread.
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InflateMode { Auto, Stream, Buffer }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorArg { Auto, Always, Never }

pub struct Args {
    pub patterns: Vec<String>,
    pub paths: Vec<PathBuf>,
    pub globs: Vec<String>,
    pub jobs: usize,
    pub nice: Option<i32>,
    pub drop_cache: bool,
    pub inflate: InflateMode,
    pub inflate_budget: u64,
    pub load_limit: Option<f64>,
    pub archive_sep: char,
    pub sort: bool,
    pub list_members: bool,
    pub count: bool,
    pub files_with_matches: bool,
    pub quiet: bool,
    pub json: bool,
    pub color: ColorArg,
    pub search: search::SearchOpts,
    pub print: print::PrintOpts,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            patterns: Vec::new(),
            paths: Vec::new(),
            globs: Vec::new(),
            jobs: 1,
            nice: Some(10),
            drop_cache: true,
            inflate: InflateMode::Auto,
            inflate_budget: 64 * 1024 * 1024,
            load_limit: None,
            archive_sep: ':',
            sort: true,
            list_members: false,
            count: false,
            files_with_matches: false,
            quiet: false,
            json: false,
            color: ColorArg::Auto,
            search: search::SearchOpts::default(),
            print: print::PrintOpts::default(),
        }
    }
}

/// `512K`, `2M`, `1G`, or a bare byte count.
fn parse_size(s: &str) -> anyhow::Result<u64> {
    let s = s.trim();
    let (num, mult) = match s.chars().last() {
        Some('K') | Some('k') => (&s[..s.len() - 1], 1024),
        Some('M') | Some('m') => (&s[..s.len() - 1], 1024 * 1024),
        Some('G') | Some('g') => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        _ => (s, 1),
    };
    let n: u64 = num.parse()?;
    n.checked_mul(mult)
        .ok_or_else(|| anyhow::anyhow!("--inflate-budget value too large: {s}"))
}

pub fn parse() -> anyhow::Result<Args> {
    parse_from(std::env::args_os())
}

pub fn parse_from<I>(argv: I) -> anyhow::Result<Args>
where
    I: IntoIterator<Item = OsString>,
{
    use lexopt::prelude::*;

    let mut a = Args::default();
    let mut explicit_jobs = false;
    let mut explicit_nice = false;
    let mut ctx_both: Option<usize> = None;
    // Positionals are banked rather than assigned as they arrive, so the rule
    // that splits PATTERN from PATH does not depend on whether `-T` or `-e`
    // happened to come before or after them on the command line.
    let mut positionals: Vec<OsString> = Vec::new();
    let mut explicit_patterns = false;

    let mut p = lexopt::Parser::from_iter(argv);
    while let Some(arg) = p.next()? {
        match arg {
            Short('e') | Long("regexp") => {
                a.patterns.push(p.value()?.string()?);
                explicit_patterns = true;
            }
            Short('i') | Long("ignore-case") => a.search.case_insensitive = true,
            Short('S') | Long("smart-case") => a.search.smart_case = true,
            Short('w') | Long("word-regexp") => a.search.word = true,
            Short('F') | Long("fixed-strings") => a.search.fixed = true,
            Short('v') | Long("invert-match") => a.search.invert = true,
            Short('a') | Long("text") => a.search.text = true,
            Short('m') | Long("max-count") => {
                a.search.max_count = Some(p.value()?.parse()?)
            }
            Long("crlf") => a.search.crlf = true,

            Short('n') | Long("line-number") => a.search.line_numbers = true,
            Short('N') | Long("no-line-number") => a.search.line_numbers = false,
            Short('A') | Long("after") | Long("after-context") => {
                a.search.after = p.value()?.parse()?
            }
            Short('B') | Long("before") | Long("before-context") => {
                a.search.before = p.value()?.parse()?
            }
            Short('C') | Long("context") => ctx_both = Some(p.value()?.parse()?),
            Short('o') | Long("only-matching") => a.print.only_matching = true,
            Short('c') | Long("count") => a.count = true,
            Short('l') | Long("files-with-matches") => a.files_with_matches = true,
            Short('q') | Long("quiet") => a.quiet = true,
            Long("json") => a.json = true,
            Long("color") => {
                a.color = match p.value()?.string()?.as_str() {
                    "auto" => ColorArg::Auto,
                    "always" => ColorArg::Always,
                    "never" => ColorArg::Never,
                    other => anyhow::bail!("--color expects auto|always|never, got '{other}'"),
                }
            }

            Short('g') | Long("glob") => a.globs.push(p.value()?.string()?),
            Short('T') | Long("list-members") => a.list_members = true,
            Long("archive-sep") => {
                let v = p.value()?.string()?;
                let mut it = v.chars();
                match (it.next(), it.next()) {
                    (Some(c), None) => a.archive_sep = c,
                    _ => anyhow::bail!("--archive-sep expects exactly one character"),
                }
            }
            Long("no-sort") => a.sort = false,

            Short('j') | Long("jobs") => {
                a.jobs = p.value()?.parse()?;
                explicit_jobs = true;
            }
            Long("turbo") => {
                if !explicit_jobs { a.jobs = num_cpus::get(); }
                if !explicit_nice { a.nice = Some(0); }
            }
            Long("nice") => {
                a.nice = Some(p.value()?.parse()?);
                explicit_nice = true;
            }
            Long("no-nice") => {
                a.nice = None;
                explicit_nice = true;
            }
            Long("no-drop-cache") => a.drop_cache = false,
            Long("inflate") => {
                a.inflate = match p.value()?.string()?.as_str() {
                    "auto" => InflateMode::Auto,
                    "stream" => InflateMode::Stream,
                    "buffer" => InflateMode::Buffer,
                    other => anyhow::bail!("--inflate expects auto|stream|buffer, got '{other}'"),
                }
            }
            Long("inflate-budget") => a.inflate_budget = parse_size(&p.value()?.string()?)?,
            Long("load-limit") => a.load_limit = Some(p.value()?.parse()?),

            Short('h') | Long("help") => {
                print!("{HELP}");
                std::process::exit(0);
            }
            Short('V') | Long("version") => {
                println!("trg {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }

            Value(v) => positionals.push(v),
            other => return Err(other.unexpected().into()),
        }
    }

    // -j given explicitly after --turbo must win; --turbo already respected
    // explicit_jobs, so nothing more is needed here.
    if let Some(n) = ctx_both {
        a.search.before = n;
        a.search.after = n;
    }
    // Collapse the output-mode flags into exactly one mode here, so nothing
    // downstream has to re-derive the precedence. Most suppressive wins: `-q`
    // asked for no output at all, and honouring the noisier flag over it would
    // be the wrong way to resolve a contradiction.
    a.print.mode = if a.quiet {
        print::Mode::Quiet
    } else if a.files_with_matches {
        print::Mode::FilesWithMatches
    } else if a.count {
        print::Mode::Count
    } else if a.json {
        print::Mode::Json
    } else {
        print::Mode::Standard
    };

    // Split the banked positionals into PATTERN and PATH.
    //
    // `-T` asks a question about the tar headers, not about the members'
    // contents: `list_members` never consults the matcher at all. So it takes
    // no pattern *whatsoever* and every positional is a path. The earlier rule
    // — "exactly one positional is a path" — closed `trg -T archive.tgz` but
    // not `trg -T *.tgz`, where the shell supplies several and the first
    // archive was silently eaten as a pattern it would never be read for. An
    // argument the mode cannot use has no unambiguous reading, so it does not
    // get one; `trg -T -e pat a.tgz` still parses, because `-e` is explicit.
    //
    // Otherwise the first positional is the pattern unless `-e` already
    // supplied one, matching ripgrep.
    let mut rest = positionals.into_iter();
    if !a.list_members
        && !explicit_patterns
        && let Some(first) = rest.next()
    {
        a.patterns.push(first.string()?);
    }
    a.paths.extend(rest.map(PathBuf::from));

    // `-T` is the one mode with no pattern to be missing.
    if a.patterns.is_empty() && !a.list_members {
        anyhow::bail!("no pattern given\n\n{HELP}");
    }
    // A run with nothing to read must never reach the exit-code contract: with
    // no paths the job list is empty, the scheduler returns immediately, and
    // `matched: false, partial: false` is a 1 — "I read everything and the logs
    // are clean" for a run that opened nothing. `trg needle` with the path
    // forgotten, and `trg needle $LOGS` with $LOGS unset, both landed there.
    // Searching the working directory instead, as ripgrep does, would be worse:
    // a confident 1 from a directory nobody named.
    if a.paths.is_empty() {
        anyhow::bail!("no path given: name at least one archive or directory\n\n{HELP}");
    }
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Args {
        let argv: Vec<std::ffi::OsString> =
            std::iter::once("trg".into()).chain(args.iter().map(|s| s.into())).collect();
        parse_from(argv).unwrap()
    }

    #[test]
    fn defaults_match_the_spec() {
        let a = p(&["pat", "f.tgz"]);
        assert_eq!(a.jobs, 1);
        assert_eq!(a.nice, Some(10));
        assert!(a.drop_cache);
        assert!(matches!(a.inflate, InflateMode::Auto));
        assert_eq!(a.inflate_budget, 64 * 1024 * 1024);
        assert_eq!(a.archive_sep, ':');
        assert!(a.sort);
        assert_eq!(a.search.heap_limit, 16 * 1024 * 1024);
        assert!(a.load_limit.is_none());
    }

    #[test]
    fn first_positional_is_the_pattern_rest_are_paths() {
        let a = p(&["NEEDLE", "a.tgz", "b.tgz"]);
        assert_eq!(a.patterns, vec!["NEEDLE"]);
        assert_eq!(a.paths.len(), 2);
    }

    #[test]
    fn dash_e_patterns_do_not_consume_the_first_positional() {
        let a = p(&["-e", "one", "-e", "two", "a.tgz"]);
        assert_eq!(a.patterns, vec!["one", "two"]);
        assert_eq!(a.paths.len(), 1);
    }

    #[test]
    fn turbo_raises_jobs_and_clears_nice() {
        let a = p(&["--turbo", "pat", "f.tgz"]);
        assert_eq!(a.jobs, num_cpus::get());
        assert_eq!(a.nice, Some(0));
        assert!(a.drop_cache, "cache dropping stays on under --turbo");
    }

    #[test]
    fn explicit_j_after_turbo_wins() {
        let a = p(&["--turbo", "-j", "2", "pat", "f.tgz"]);
        assert_eq!(a.jobs, 2);
    }

    #[test]
    fn no_nice_disables_priority_lowering() {
        let a = p(&["--no-nice", "pat", "f.tgz"]);
        assert_eq!(a.nice, None);
    }

    #[test]
    fn context_flags_match_rg_semantics() {
        let a = p(&["-C", "3", "pat", "f.tgz"]);
        assert_eq!((a.search.before, a.search.after), (3, 3));
        let a = p(&["-A", "2", "-B", "1", "pat", "f.tgz"]);
        assert_eq!((a.search.before, a.search.after), (1, 2));
    }

    #[test]
    fn n_capital_disables_line_numbers() {
        assert!(p(&["pat", "f.tgz"]).search.line_numbers);
        assert!(!p(&["-N", "pat", "f.tgz"]).search.line_numbers);
    }

    #[test]
    fn size_suffixes_parse() {
        assert_eq!(p(&["--inflate-budget", "512K", "p", "f"]).inflate_budget, 512 * 1024);
        assert_eq!(p(&["--inflate-budget", "2M", "p", "f"]).inflate_budget, 2 * 1024 * 1024);
        assert_eq!(p(&["--inflate-budget", "1G", "p", "f"]).inflate_budget, 1024 * 1024 * 1024);
        assert_eq!(p(&["--inflate-budget", "1024", "p", "f"]).inflate_budget, 1024);
    }

    #[test]
    fn globs_accumulate() {
        let a = p(&["-g", "*.access.log", "-g", "*.error.log", "pat", "f.tgz"]);
        assert_eq!(a.globs.len(), 2);
    }

    #[test]
    fn archive_sep_is_configurable() {
        assert_eq!(p(&["--archive-sep", "!", "pat", "f.tgz"]).archive_sep, '!');
    }

    #[test]
    fn unknown_flag_is_an_error_not_a_pattern() {
        let argv: Vec<std::ffi::OsString> =
            ["trg", "--nonsense", "pat", "f.tgz"].iter().map(|s| s.into()).collect();
        assert!(parse_from(argv).is_err());
    }

    #[test]
    fn inflate_mode_rejects_garbage() {
        let argv: Vec<std::ffi::OsString> =
            ["trg", "--inflate", "sideways", "p", "f"].iter().map(|s| s.into()).collect();
        assert!(parse_from(argv).is_err());
    }

    #[test]
    fn crlf_flag_sets_the_search_opt_and_defaults_to_false() {
        assert!(!p(&["pat", "f.tgz"]).search.crlf);
        assert!(p(&["--crlf", "pat", "f.tgz"]).search.crlf);
    }

    #[test]
    fn inflate_budget_overflow_is_an_error_not_a_wrapped_value() {
        let argv: Vec<std::ffi::OsString> =
            ["trg", "--inflate-budget", "18446744073709551615G", "p", "f"]
                .iter().map(|s| s.into()).collect();
        assert!(parse_from(argv).is_err());
    }

    #[test]
    fn output_mode_precedence_is_most_suppressive_wins() {
        assert_eq!(p(&["pat", "f"]).print.mode, print::Mode::Standard);
        assert_eq!(p(&["-c", "pat", "f"]).print.mode, print::Mode::Count);
        assert_eq!(p(&["-l", "pat", "f"]).print.mode, print::Mode::FilesWithMatches);
        assert_eq!(p(&["-q", "pat", "f"]).print.mode, print::Mode::Quiet);
        assert_eq!(p(&["--json", "pat", "f"]).print.mode, print::Mode::Json);
        assert_eq!(p(&["-c", "-l", "pat", "f"]).print.mode, print::Mode::FilesWithMatches);
        assert_eq!(p(&["-c", "-l", "-q", "pat", "f"]).print.mode, print::Mode::Quiet);
        assert_eq!(p(&["--json", "-c", "pat", "f"]).print.mode, print::Mode::Count);
    }

    #[test]
    fn color_parses_all_three_and_rejects_the_rest() {
        assert_eq!(p(&["pat", "f"]).color, ColorArg::Auto);
        assert_eq!(p(&["--color", "always", "pat", "f"]).color, ColorArg::Always);
        assert_eq!(p(&["--color", "never", "pat", "f"]).color, ColorArg::Never);
        let argv: Vec<std::ffi::OsString> =
            ["trg", "--color", "nonsense", "pat", "f"].iter().map(|s| s.into()).collect();
        assert!(parse_from(argv).is_err());
    }

    /// The trap this fix removes: `-T` needs no pattern, but the positional
    /// rule gave the archive path to the pattern slot, leaving no paths at all.
    /// The command exited 1 in silence — the first natural use of a documented
    /// flag doing nothing.
    #[test]
    fn dash_t_with_one_positional_takes_it_as_a_path_not_a_pattern() {
        let a = p(&["-T", "archive.tgz"]);
        assert!(a.patterns.is_empty(), "-T needs no pattern, got {:?}", a.patterns);
        assert_eq!(a.paths, vec![PathBuf::from("archive.tgz")]);
    }

    /// The trap the one-positional rule did not close: a shell glob supplies
    /// several positionals, and the first archive was eaten as a pattern `-T`
    /// never reads. Under `-T` every positional is a path, no exceptions.
    #[test]
    fn dash_t_takes_every_positional_as_a_path() {
        let a = p(&["-T", "a.tgz", "b.tgz", "c.tgz"]);
        assert!(a.patterns.is_empty(), "-T takes no pattern, got {:?}", a.patterns);
        assert_eq!(
            a.paths,
            vec![PathBuf::from("a.tgz"), PathBuf::from("b.tgz"), PathBuf::from("c.tgz")]
        );
    }

    /// Flag order must not change the reading: `-T` after the positionals is
    /// the same command.
    #[test]
    fn dash_t_after_the_positionals_parses_the_same() {
        let a = p(&["a.tgz", "b.tgz", "-T"]);
        assert!(a.patterns.is_empty(), "got {:?}", a.patterns);
        assert_eq!(a.paths, vec![PathBuf::from("a.tgz"), PathBuf::from("b.tgz")]);
    }

    /// `-e` after a positional still makes that positional a path, so the
    /// pattern slot is decided by the whole command line rather than by which
    /// argument arrived first.
    #[test]
    fn an_explicit_e_makes_every_positional_a_path_whatever_the_order() {
        let a = p(&["a.tgz", "-e", "pat", "b.tgz"]);
        assert_eq!(a.patterns, vec!["pat"]);
        assert_eq!(a.paths, vec![PathBuf::from("a.tgz"), PathBuf::from("b.tgz")]);
    }

    /// `-e` fills the pattern slot explicitly, so the lone positional is a path
    /// for the ordinary reason and the `-T` rule must not double-move it.
    #[test]
    fn dash_t_with_an_explicit_e_pattern_keeps_both() {
        let a = p(&["-T", "-e", "pat", "archive.tgz"]);
        assert_eq!(a.patterns, vec!["pat"]);
        assert_eq!(a.paths, vec![PathBuf::from("archive.tgz")]);
    }

    fn err(args: &[&str]) -> String {
        let argv: Vec<std::ffi::OsString> =
            std::iter::once("trg".into()).chain(args.iter().map(|s| s.into())).collect();
        match parse_from(argv) {
            Ok(_) => panic!("expected a usage error, got a parse"),
            Err(e) => format!("{e:#}"),
        }
    }

    /// Only `-T` is exempt. Everything else still refuses to run patternless
    /// rather than searching for the empty string.
    #[test]
    fn a_missing_pattern_is_still_an_error_without_dash_t() {
        assert!(err(&[]).contains("no pattern given"));
        assert!(err(&["-e", "pat"]).contains("no path given"), "pattern present, path missing");
    }

    /// The blocker: with no path there is nothing to read, and a run that read
    /// nothing must not be able to reach the exit-code contract — an empty job
    /// list produced `matched: false, partial: false`, which is a 1.
    #[test]
    fn a_missing_path_is_a_usage_error_not_an_empty_clean_run() {
        assert!(err(&["needle"]).contains("no path given"), "pattern with no path");
        assert!(err(&["-T"]).contains("no path given"), "-T with no path");
        assert!(err(&["-q", "needle"]).contains("no path given"), "-q must not hide it");
    }

    /// A pattern is still a pattern, so this is the *path* that is missing —
    /// the realistic form is `trg needle $LOGS` with `$LOGS` unset, which the
    /// shell collapses to exactly this.
    #[test]
    fn one_positional_is_the_pattern_and_leaves_no_path() {
        assert!(err(&["archive.tgz"]).contains("no path given"));
    }

    #[test]
    fn inflate_budget_accepts_u64_max_with_no_suffix() {
        let a = p(&["--inflate-budget", &u64::MAX.to_string(), "p", "f"]);
        assert_eq!(a.inflate_budget, u64::MAX);
    }

    #[test]
    fn inflate_budget_accepts_a_large_value_with_a_suffix_just_under_the_boundary() {
        // u64::MAX / 1024^3, so the multiply by G does not overflow.
        let n = u64::MAX / (1024 * 1024 * 1024);
        let a = p(&["--inflate-budget", &format!("{n}G"), "p", "f"]);
        assert_eq!(a.inflate_budget, n * 1024 * 1024 * 1024);
    }
}
