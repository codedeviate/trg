#!/bin/sh
# Runs the torture suite against a release build, from the repository root:
#
#     bench/torture/run.sh          # fixtures, cut, flip, edges, parallel, diff
#     bench/torture/run.sh full     # and the size limits and bombs (minutes)
#
# Env: TRG (default target/release/trg, built if missing), WORK (default
# target/torture). Each script exits non-zero on a result it does not
# expect; this runs them all and exits 1 if any did.
set -u
cd "$(dirname "$0")/../.."
if [ -z "${TRG:-}" ]; then
    cargo build -q --release || exit 1
fi
here=bench/torture
sh $here/fixtures.sh >/dev/null || exit 1
status=0
step() {
    echo "== $1"
    shift
    python3 "$@" || status=1
}
step "truncation" $here/cut.py 100
step "corruption" $here/flip.py 100
step "edge cases" $here/edges.py
step "parallel" $here/parallel.py
command -v rg >/dev/null && step "differential (rg)" $here/diff.py 1000 || echo "== differential: no rg, skipped"
[ "${1:-}" = full ] && step "size limits" $here/bombs.py
exit $status
