//! `trg --examples`: worked examples of every option, printed as text.
//!
//! This is living documentation, and `tests/examples.rs` keeps it alive. It
//! builds the `logs/` tree described in [`FILES`], runs the trg part of every
//! example against it, and checks the exit code each one claims. It also fails
//! when an option in `--help` has no example. So a new flag, a renamed one, or
//! a change in what a command does breaks the build until this file says so.

/// One example: a shell line and what it does.
pub struct Example {
    /// What to type. The first `trg …` in it, up to the first unquoted `|`,
    /// `;`, `&`, `>`, `<`, `)` or newline, is what the test runs; anything
    /// around it is shell the reader can copy as it stands.
    pub cmd: &'static str,
    /// The exit code that trg part gives against the tree in [`FILES`].
    pub exit: i32,
    /// Plain prose, wrapped when printed.
    pub says: &'static str,
}

pub struct Section {
    pub title: &'static str,
    pub intro: &'static str,
    pub examples: &'static [Example],
}

/// A file in the example tree.
pub struct File {
    pub path: &'static str,
    /// What `trg -T` lists for it; empty for a file that is not an archive.
    pub members: &'static [&'static str],
    pub note: &'static str,
}

/// The tree every example searches. `tests/examples.rs` builds exactly this.
pub const FILES: &[File] = &[
    File {
        path: "logs/web-2026-08-23.tgz",
        members: &["access.log", "access.log.1.gz", "error.log", "core.811"],
        note: "Apache logs, one rotated, and a core dump",
    },
    File { path: "logs/web-2026-08-24.tar.zst", members: &["access.log", "error.log"], note: "" },
    File {
        path: "logs/2026-07/web-2026-07-31.tar.xz",
        members: &["access.log", "error.log"],
        note: "",
    },
    File {
        path: "logs/app-2026-08-24.zip",
        members: &["app/server.log", "app/worker.log"],
        note: "",
    },
    File { path: "logs/mail-2026-08-24.tar.bz2", members: &["maillog"], note: "" },
    File { path: "logs/db-2026-08-24.tar.lz4", members: &["slow.log"], note: "" },
    File {
        path: "legacy/web-2019-03-01.tar.Z",
        members: &["access.log"],
        note: "Unix compress",
    },
    File {
        path: "legacy/web-2019-02-28.tgz",
        members: &["access.log", "access.log.1.Z"],
        note: "rotated with compress",
    },
    File {
        path: "logs/iis-2026-08-24.zip",
        members: &["u_ex260824.log"],
        note: "Windows line endings",
    },
    File { path: "current/access.log", members: &[], note: "today's log, not compressed" },
    File { path: "incoming/download.bin", members: &["access.log"], note: "a .tgz by another name" },
    File { path: "broken/web-2026-08-25.tgz", members: &[], note: "cut off halfway through" },
];

pub const SECTIONS: &[Section] = &[
    Section {
        title: "FIRST SEARCHES",
        intro: "A pattern, then one or more archives or directories.",
        examples: &[
            Example {
                cmd: "trg 'POST /checkout' logs/web-2026-08-23.tgz",
                exit: 0,
                says: "Search one archive. Each hit prints as archive:member:line:text, \
                       so you always know which file inside which archive it came from.",
            },
            Example {
                cmd: "trg 'POST /checkout' logs/*.tgz logs/*.tar.zst",
                exit: 0,
                says: "Several archives. Output comes in the order the archives were \
                       named, whatever order they finish in.",
            },
            Example {
                cmd: "trg 'POST /checkout' logs/",
                exit: 0,
                says: "A directory is walked recursively and every file in it is \
                       searched. Each file's format is detected from its content, so \
                       gzip, zstd, xz, bzip2, lz4, zip and plain files can sit side by \
                       side.",
            },
            Example {
                cmd: "trg ' 500 ' current/access.log logs/",
                exit: 0,
                says: "Uncompressed files are searched too, so today's live log and the \
                       archived days go in one command.",
            },
            Example {
                cmd: "trg 'POST /checkout' incoming/download.bin",
                exit: 0,
                says: "The file name does not matter: this .tgz is read as one because \
                       of what is in it. There is no -z flag to forget.",
            },
            Example {
                cmd: "trg --examples | less",
                exit: 0,
                says: "This text. It is checked against trg on every build, so it \
                       describes the version that printed it.",
            },
            Example { cmd: "trg -V", exit: 0, says: "Which version you have." },
        ],
    },
    Section {
        title: "PATTERNS",
        intro: "Patterns are regular expressions with ripgrep's syntax and flags.",
        examples: &[
            Example {
                cmd: "trg -i 'timeout' logs/",
                exit: 0,
                says: "Ignore case: finds Timeout, TIMEOUT and timeout.",
            },
            Example {
                cmd: "trg -S 'timeout' logs/",
                exit: 0,
                says: "Smart case: an all-lowercase pattern ignores case, and one with \
                       an uppercase letter (-S 'Timeout') matches it exactly.",
            },
            Example {
                cmd: "trg -w '4711' logs/",
                exit: 0,
                says: "Whole words only: finds order 4711 in /api/orders/4711 and \
                       order=4711, but not job 47110.",
            },
            Example {
                cmd: "trg -F '[proxy:error]' logs/",
                exit: 0,
                says: "Fixed string: the brackets are literal. Without -F they would be \
                       a character class matching any one of the letters inside.",
            },
            Example {
                cmd: "trg -e 'PHP Fatal' -e 'Segmentation fault' logs/",
                exit: 0,
                says: "Several patterns: a line matching any of them is a hit. With -e, \
                       every positional argument is a path.",
            },
            Example {
                cmd: "trg -e '--dry-run' logs/app-2026-08-24.zip",
                exit: 0,
                says: "-e also takes a pattern that starts with a dash, which would \
                       otherwise be read as a flag.",
            },
            Example {
                cmd: "trg '\" 5[0-9]{2} ' logs/",
                exit: 0,
                says: "Any 5xx status in an Apache access log: the status follows the \
                       closing quote of the request.",
            },
            Example {
                cmd: "trg '^10\\.0\\.0\\.7 ' logs/",
                exit: 0,
                says: "Everything one client did. ^ anchors at the start of the line, \
                       and the escaped dots match only dots.",
            },
            Example {
                cmd: "trg '24/Aug/2026:10:0[0-4]' logs/",
                exit: 0,
                says: "A time window: 10:00 to 10:04 on 24 August, in Apache's \
                       timestamp format.",
            },
            Example {
                cmd: "trg -v '\" 200 ' logs/web-2026-08-23.tgz",
                exit: 0,
                says: "Invert: every line that is not a 200. A member counts as a hit \
                       when it has at least one such line.",
            },
            Example {
                cmd: "trg --crlf ' [0-9]{4,}$' logs/iis-2026-08-24.zip",
                exit: 0,
                says: "Windows logs end lines with \\r\\n. --crlf lets $ match before \
                       the \\r. This finds IIS requests whose last field, time-taken, \
                       is 1000 ms or more.",
            },
            Example {
                cmd: "trg ' [0-9]{4,}$' logs/iis-2026-08-24.zip",
                exit: 1,
                says: "The same search without --crlf finds nothing: every line ends in \
                       \\r, so $ never follows a digit.",
            },
            Example {
                cmd: "trg -a 'Segmentation fault' -g 'core.*' logs/web-2026-08-23.tgz",
                exit: 0,
                says: "Search binary members as text. Without -a a member with NUL \
                       bytes, like this core dump, is skipped and the same command \
                       exits 1.",
            },
        ],
    },
    Section {
        title: "CHOOSING MEMBERS",
        intro: "-g filters the files inside archives. It never picks archives; \
                --archive-glob does that.",
        examples: &[
            Example {
                cmd: "trg -T logs/web-2026-08-23.tgz",
                exit: 0,
                says: "List the members without searching. -T takes no pattern: every \
                       argument is a path, so trg -T logs/*.tgz lists all of them.",
            },
            Example {
                cmd: "trg -T -g '*.gz' logs/",
                exit: 0,
                says: "-T honours -g, so this shows which members a -g would search.",
            },
            Example {
                cmd: "trg -g 'error.log' 'Timeout' logs/",
                exit: 0,
                says: "Only members named error.log. A glob without a / matches the file \
                       name at any depth inside the archive.",
            },
            Example {
                cmd: "trg -g 'app/*' 'ERROR' logs/app-2026-08-24.zip",
                exit: 0,
                says: "A glob with a / matches the whole member path.",
            },
            Example {
                cmd: "trg -g 'access.log*' '^10\\.0\\.0\\.7 ' logs/web-2026-08-23.tgz",
                exit: 0,
                says: "access.log and its rotated copies. access.log.1.gz is gzip inside \
                       the archive; trg decompresses it and searches it as text.",
            },
            Example {
                cmd: "trg -g '!core.*' 'Segmentation fault' logs/",
                exit: 0,
                says: "A leading ! excludes. Here: everything but core dumps.",
            },
            Example {
                cmd: "trg -g '*.log' -g '!worker.log' 'ERROR' logs/",
                exit: 0,
                says: "Several globs: the last one that matches a member decides. So \
                       every .log except worker.log.",
            },
        ],
    },
    Section {
        title: "CHOOSING ARCHIVES",
        intro: "--archive-glob picks which files a directory walk searches.",
        examples: &[
            Example {
                cmd: "trg --archive-glob 'web-2026-08-*' ' 500 ' logs/",
                exit: 0,
                says: "Only August's web archives. A glob without a / matches the file \
                       name at any depth below logs/.",
            },
            Example {
                cmd: "trg --archive-glob '2026-07/*' ' 500 ' logs/",
                exit: 0,
                says: "A glob with a / matches the path below the directory named: here, \
                       the files in logs/2026-07/.",
            },
            Example {
                cmd: "trg --archive-glob '!iis-*' ' 200 ' logs/",
                exit: 0,
                says: "Everything except the IIS archives.",
            },
            Example {
                cmd: "trg --archive-glob 'web-2025-*' ' 500 ' logs/",
                exit: 2,
                says: "A glob that matches no file is exit 2, not a quiet 1: a typo in \
                       the date must not look like a clean year.",
            },
            Example {
                cmd: "trg --archive-glob '*.zip' ' 500 ' logs/ current/access.log",
                exit: 0,
                says: "Files named on the command line are always searched; the glob \
                       only filters what a directory walk finds.",
            },
        ],
    },
    Section {
        title: "OUTPUT",
        intro: "Output flags mean what they mean in ripgrep.",
        examples: &[
            Example {
                cmd: "trg -C 2 'PHP Fatal' logs/",
                exit: 0,
                says: "Two lines of context before and after each hit.",
            },
            Example {
                cmd: "trg -B 3 -A 1 'Segmentation fault' logs/",
                exit: 0,
                says: "Three lines before and one after: what led up to the crash.",
            },
            Example {
                cmd: "trg -N 'sqlmap' logs/",
                exit: 0,
                says: "No line numbers: archive:member:text.",
            },
            Example {
                cmd: "trg -n 'sqlmap' logs/",
                exit: 0,
                says: "Line numbers are on by default. -n turns them back on after an \
                       -N, say one in a shell alias.",
            },
            Example {
                cmd: "trg -c '\" 5[0-9]{2} ' -g 'access.log*' logs/",
                exit: 0,
                says: "Count 5xx responses per member, one line each as \
                       archive:member:count.",
            },
            Example {
                cmd: "trg -c -o 'order=[0-9]+' logs/app-2026-08-24.zip",
                exit: 0,
                says: "With -o, -c counts every match rather than every matching line, \
                       as rg does: a line naming two orders counts twice.",
            },
            Example {
                cmd: "trg -l 'sqlmap' logs/",
                exit: 0,
                says: "Only the archive:member names that have a hit.",
            },
            Example {
                cmd: "trg -m 1 'Timeout' logs/",
                exit: 0,
                says: "Stop after the first hit in each member: was it there at all, \
                       and where did it start?",
            },
            Example {
                cmd: "trg -m 1 -A 2 'payment declined' logs/app-2026-08-24.zip",
                exit: 0,
                says: "The lines after the last allowed hit are still printed, as in \
                       rg. -A and -B win over -C whichever comes first.",
            },
            Example {
                cmd: "trg -o -N '^[0-9.]+' -g 'access.log*' logs/ |\n  \
                      awk -F: '{print $NF}' | sort | uniq -c | sort -rn",
                exit: 0,
                says: "Top client IPs. -o prints only the match, so each line ends in \
                       an address and awk takes the last field.",
            },
            Example {
                cmd: "trg -l --archive-sep '|' 'sqlmap' logs/ | cut -d'|' -f1 | sort -u",
                exit: 0,
                says: "Which archives (not members) have a hit. --archive-sep changes \
                       the separator between archive and member, so it cannot clash \
                       with a : in a member name.",
            },
            Example {
                cmd: "trg --json 'PHP Fatal' logs/ |\n  \
                      jq -r 'select(.type == \"match\") | .data.path.text'",
                exit: 0,
                says: "JSON Lines in ripgrep's format, for scripts. The path field is \
                       archive:member.",
            },
            Example {
                cmd: "trg --color always ' 500 ' logs/ | less -R",
                exit: 0,
                says: "Keep colours when paging. By default colour is used only on a \
                       terminal (--color auto); --color never turns it off.",
            },
            Example {
                cmd: "trg --escape always 'owned' logs/web-2026-08-24.tar.zst",
                exit: 0,
                says: "A log can hold terminal escape sequences, for example planted in \
                       a User-Agent. On a terminal trg shows them as text (\\x1b) rather \
                       than letting them act. --escape always does that even when piped; \
                       --escape never sends the bytes raw.",
            },
            Example {
                cmd: "trg -j 4 --no-sort -l 'sqlmap' logs/",
                exit: 0,
                says: "Print each archive's results as soon as it finishes rather than \
                       in the order given. Only matters with -j above 1.",
            },
        ],
    },
    Section {
        title: "FORMATS",
        intro: "Compression is detected from content. Every format below is a \
                plain trg PATTERN PATH.",
        examples: &[
            Example {
                cmd: "trg 'ERROR' logs/app-2026-08-24.zip",
                exit: 0,
                says: "zip: members are searched like a tar's, compressed or not. An \
                       encrypted or damaged member is reported and makes the run exit 2; \
                       the other members are still searched.",
            },
            Example {
                cmd: "trg 'status=bounced' logs/mail-2026-08-24.tar.bz2",
                exit: 0,
                says: "bzip2, including the multi-stream files pbzip2 writes.",
            },
            Example {
                cmd: "trg 'Query_time: [1-9]' logs/db-2026-08-24.tar.lz4",
                exit: 0,
                says: "lz4: MySQL slow queries over a second.",
            },
            Example {
                cmd: "trg '^10\\.0\\.0\\.7 ' logs/2026-07/web-2026-07-31.tar.xz",
                exit: 0,
                says: "xz. zstd (.tar.zst) and gzip (.tgz) work the same way.",
            },
            Example {
                cmd: "trg 'POST /checkout' legacy/web-2019-03-01.tar.Z",
                exit: 1,
                says: ".Z (Unix compress) has no length and no checksum, so a cut-off \
                       file reads as a shorter one. A .Z with no match is still exit 1, \
                       with a note on stderr that the file could not vouch for its end.",
            },
            Example {
                cmd: "trg 'cart.php' legacy/web-2019-02-28.tgz",
                exit: 0,
                says: "A rotated log inside an archive is unwrapped one level, whether \
                       gzip, zstd, xz, bzip2, lz4 or .Z, and searched with its own \
                       line numbers: here access.log.1.Z.",
            },
        ],
    },
    Section {
        title: "EXIT CODES AND SCRIPTS",
        intro: "0: matches, everything read. 1: no matches, everything read. \
                2: something went unread. Never trust a 1 you did not get.",
        examples: &[
            Example {
                cmd: "trg -q 'sqlmap' logs/\n\
                      case $? in\n  \
                        0) echo 'scanner seen' ;;\n  \
                        1) echo 'clean' ;;\n  \
                        *) echo 'some logs were not read' >&2 ;;\n\
                      esac",
                exit: 0,
                says: "-q prints nothing; the exit code is the answer. Handle 2 apart \
                       from 1: if trg -q ...; then treats an unread archive as clean.",
            },
            Example {
                cmd: "trg ' 500 ' broken/web-2026-08-25.tgz",
                exit: 2,
                says: "A truncated archive: the hits before the damage are printed, the \
                       error goes to stderr with the archive's name, and the exit code \
                       is 2. Every checksum the format carries is read, the ones after \
                       the tar's end included, so a corrupted archive is a 2 as well.",
            },
            Example {
                cmd: "trg ' 500 ' logs/ broken/",
                exit: 2,
                says: "One bad archive makes the whole run a 2, but every other archive \
                       is still searched and printed.",
            },
            Example {
                cmd: "trg 'POST /checkout' $LOGS",
                exit: 2,
                says: "If $LOGS is unset there is no path, and that is an error. trg \
                       never falls back to searching the current directory.",
            },
            Example {
                cmd: "trg ' 500 ' logs/ | head -n 3",
                exit: 0,
                says: "When the reader stops early, trg stops too and exits 0. The \
                       archives it had not reached are left unread.",
            },
        ],
    },
    Section {
        title: "BIG SWEEPS AND BUSY SERVERS",
        intro: "By default trg reads one archive at a time at nice 10 and idle I/O \
                priority, and drops each archive from the page cache once it is \
                read, so a sweep does not push the server's own data out of memory.",
        examples: &[
            Example {
                cmd: "trg -j 4 ' 500 ' logs/",
                exit: 0,
                says: "Four archives at once. Output still comes in the order given.",
            },
            Example {
                cmd: "trg --turbo -c ' 500 ' logs/",
                exit: 0,
                says: "One job per CPU at normal priority: for a machine that is yours \
                       alone. Reads stay at idle I/O priority and the page cache is \
                       still dropped. -j still sets the job count.",
            },
            Example {
                cmd: "trg -j 8 --load-limit 6 ' 500 ' logs/",
                exit: 0,
                says: "Up to eight jobs, but drop to one whenever the load from other \
                       processes passes 6, and add jobs back as it falls. Checked every \
                       5 seconds.",
            },
            Example {
                cmd: "trg --nice 19 ' 500 ' logs/",
                exit: 0,
                says: "The lowest priority: only use CPU nobody else wants.",
            },
            Example {
                cmd: "trg --no-nice ' 500 ' logs/",
                exit: 0,
                says: "Keep the priority you started with.",
            },
            Example {
                cmd: "trg --no-drop-cache ' 500 ' logs/",
                exit: 0,
                says: "Leave the archives in the page cache, so searching the same \
                       archives again is faster.",
            },
        ],
    },
];

const WIDTH: usize = 78;
const CMD_INDENT: &str = "  ";
const SAYS_INDENT: &str = "      ";

/// The whole text of `trg --examples`.
pub fn render() -> String {
    let mut s = format!(
        "trg {} — examples\n\n\
         Every example searches this tree; adjust the paths to yours.\n\n",
        env!("CARGO_PKG_VERSION")
    );
    let width = FILES.iter().map(|f| f.path.len()).max().unwrap_or(0);
    for f in FILES {
        let mut what = f.members.join(", ");
        if !f.note.is_empty() {
            if !what.is_empty() {
                what.push_str(" — ");
            }
            what.push_str(f.note);
        }
        let first = format!("{CMD_INDENT}{:width$}  ", f.path);
        let rest = " ".repeat(first.len());
        wrap_into(&mut s, &what, &first, &rest);
    }
    for sec in SECTIONS {
        s.push_str(&format!("\n{}\n", sec.title));
        wrap_into(&mut s, sec.intro, "", "");
        for ex in sec.examples {
            s.push('\n');
            for line in ex.cmd.lines() {
                s.push_str(&format!("{CMD_INDENT}{line}\n"));
            }
            wrap_into(&mut s, ex.says, SAYS_INDENT, SAYS_INDENT);
        }
    }
    s
}

/// Word-wrap `text` to [`WIDTH`], starting the first line with `first` and
/// every later one with `rest`.
fn wrap_into(out: &mut String, text: &str, first: &str, rest: &str) {
    let mut line = String::from(first);
    let mut empty = true;
    for word in text.split_whitespace() {
        if !empty && line.chars().count() + 1 + word.len() > WIDTH {
            out.push_str(&line);
            out.push('\n');
            line = String::from(rest);
            empty = true;
        }
        if !empty {
            line.push(' ');
        }
        line.push_str(word);
        empty = false;
    }
    if !empty {
        out.push_str(&line);
        out.push('\n');
    }
}
