#!/usr/bin/env python3
"""Synthetic stand-ins for the three Apache logs build_corpus.sh expects.

    python3 bench/synth_logs.py OUT_DIR [SCALE]

For benchmarking where the real logs cannot go, such as a public CI runner.
Writes SE.requests, SE.log and SE.error with the real files' formats, sizes and
gzip ratios (about 15:1, 32:1 and 9:1 at SCALE=1). The ratio is what matters: it
sets how much work inflate does per output byte, which is what every timing in
compare.sh measures. Identical output for identical arguments.

The needle compare.sh searches for by default (`reqid=ZZB0RvmI4TWPrNPs5Rb`) is
written once, so trg, zgrep and rg all have exactly one hit per log copy.
"""
import random
import string
import sys
from datetime import datetime, timedelta, timezone

out = sys.argv[1]
scale = float(sys.argv[2]) if len(sys.argv) > 2 else 1.0
rng = random.Random(20260823)
ALNUM = string.ascii_letters + string.digits
NEEDLE = "ZZB0RvmI4TWPrNPs5Rb"
P_REQUESTS, P_ACCESS, P_ERROR = (float(x) for x in
                                 (sys.argv[3:6] if len(sys.argv) > 5 else (0.99, 0.99, 0.6)))


class Sticky:
    """Field values that persist from one line to the next with probability
    `p`, the way real traffic arrives in bursts from one client and session.
    Without it every line draws fresh values and gzip, whose window is 32 KB,
    almost never finds a repeat: the requests log came out at 2.7:1, not 15:1.
    `p` per file is tuned to the real ratio."""

    def __init__(self, p):
        self.p, self.prev = p, {}

    def __call__(self, name, draw):
        if name not in self.prev or rng.random() >= self.p:
            self.prev[name] = draw()
        return self.prev[name]


def token(n):
    return "".join(rng.choice(ALNUM) for _ in range(n))


def hexs(n):
    return "".join(rng.choice("0123456789abcdef") for _ in range(n))


start = datetime(2026, 7, 15, 5, 48, 0, tzinfo=timezone.utc)
ips = [f"172.19.0.{rng.randint(1, 40)}" for _ in range(12)] + ["10.0.4.17", "10.0.4.22"]
# About 200 of each in the real log.
sessions = [(token(20), hexs(32)) for _ in range(200)]
pages = ["/", "/index.html", "/__SYS__/login.html", "/admin", "/admin/", "/cart",
         "/checkout", "/search?q=shoes", "/category/men", "/category/women",
         "/product/blue-shirt", "/product/red-dress", "/api/v1/cart/items",
         "/static/app.js", "/static/app.css", "/favicon.ico", "/robots.txt"]
pages += [f"/product/item-{i}" for i in range(400)]
handlers = ["JSONHANDLER get_notifications", "JSONHANDLER cart_summary",
            "JSONHANDLER search_suggest", "JSONHANDLER stock_status"]
agents = ['"curl/8.7.1"',
          '"Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:152.0) Gecko/20100101 Firefox/152.0"',
          '"Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0 Safari/537.36"',
          '"Mozilla/5.0 (iPhone; CPU iPhone OS 19_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Mobile/15E148"',
          '"Googlebot/2.1 (+http://www.google.com/bot.html)"']


def requests(path, n, p):
    t = start
    needle_at = n // 2
    st = Sticky(p)
    rq = {}
    with open(path, "w") as f:
        for i in range(n):
            t += timedelta(seconds=rng.choice((0, 0, 0, 1, 1, 2, 7)))
            seid, sess = st("sess", lambda: rng.choice(sessions))
            admin = st("admin", lambda: ",admin_id=1" if rng.random() < 0.2 else "")
            admid = st("admid", lambda: f",admid={token(19)}") if admin else ""
            # A request logs about two lines in the real log (36,784 distinct
            # reqids in 71,133 lines), so a reqid repeats with probability 0.48.
            # The needle is written exactly once.
            if i == needle_at:
                reqid = NEEDLE
            elif i == needle_at + 1 or "reqid" not in rq or rng.random() >= 0.48:
                reqid = rq["reqid"] = token(19)
            else:
                reqid = rq["reqid"]
            what = st("what", lambda: rng.choice(handlers) if rng.random() < 0.3
                      else f"{rng.choice(('GET', 'GET', 'GET', 'POST'))} {rng.choice(pages)}")
            ip = st("ip", lambda: rng.choice(ips))
            # Brings the median line to the real 205 bytes.
            loc = st("loc", lambda: rng.choice(("lang=sv,cur=SEK,cc=SE", "lang=en,cur=EUR,cc=DE",
                                                "lang=nb,cur=NOK,cc=NO")))
            f.write(f"{t:%Y-%m-%dT%H:%M:%SZ} [shop=www,ip={ip}{admin},{loc},"
                    f"SEid={seid},Sess={sess}{admid},reqid={reqid}] ** {what}\n")


def access(path, n, p):
    t = start
    st = Sticky(p)
    with open(path, "w") as f:
        for i in range(n):
            t += timedelta(seconds=rng.choice((0, 0, 0, 1, 1, 2, 7)))
            user = st("user", lambda: "admin" if rng.random() < 0.2 else "-")
            page = st("page", lambda: rng.choice(pages))
            status, size = st("status", lambda: rng.choice(
                ((200, 38), (200, 2790), (200, 15112), (304, 0), (403, 2790), (404, 512), (302, 0))))
            ref = st("ref", lambda: rng.choice(
                ('"-"', '"https://www.neh.local/admin/admin-server-shoppacker.html"', '"https://www.neh.local/"')))
            ip = st("ip", lambda: rng.choice(ips))
            agent = st("agent", lambda: rng.choice(agents))
            method = st("method", lambda: rng.choice(("GET", "GET", "POST")))
            f.write(f"www www.neh.local {ip} - {user} [{t:%d/%b/%Y:%H:%M:%S} +0000] "
                    f'"{method} {page} HTTP/1.1" {status} {size} '
                    f'{ref} {agent} (-) "-" {1107 + i}\n')


def errors(path, n, p):
    t = start
    st = Sticky(p)
    templates = [
        "[ssl:warn] [pid {pid}:tid {pid}] AH01909: {host}:443:0 server certificate does NOT include an ID which matches the server name",
        "[php:error] [pid {pid}:tid {tid}] [client {ip}:{port}] PHP Warning:  Undefined array key \"{key}\" in /srv/app/lib/{mod}.php on line {line}",
        "[core:error] [pid {pid}:tid {tid}] [client {ip}:{port}] AH00126: Invalid URI in request GET /{junk} HTTP/1.1",
        "[proxy_fcgi:error] [pid {pid}:tid {tid}] [client {ip}:{port}] AH01071: Got error 'Primary script unknown'",
    ]
    with open(path, "w") as f:
        for _ in range(n):
            t += timedelta(seconds=rng.randint(0, 40), microseconds=rng.randint(0, 999999))
            line = st("tpl", lambda: rng.choice(templates)).format(
                pid=st("pid", lambda: rng.randint(1000, 1100)),
                tid=st("tid", lambda: rng.randint(1000, 99999)),
                host=rng.choice(("neh.local", "default")), ip=st("ip", lambda: rng.choice(ips)),
                port=st("port", lambda: rng.randint(30000, 65000)), key=rng.choice(("id", "lang", "shop", "q")),
                mod=rng.choice(("cart", "search", "product", "session")),
                line=rng.randint(10, 900), junk=st("junk", lambda: token(12)))
            f.write(f"[{t:%a %b %d %H:%M:%S.%f %Y}] {line}\n")


# Line counts from the real logs on 2026-10-23; stickiness tuned to their ratios.
requests(f"{out}/SE.requests", int(71125 * scale), P_REQUESTS)
access(f"{out}/SE.log", int(42748 * scale), P_ACCESS)
errors(f"{out}/SE.error", int(2466 * scale), P_ERROR)
