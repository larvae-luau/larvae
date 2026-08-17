#!/usr/bin/env bash
# Benchmark larvae against selene on synthetic Roblox shaped projects
#
# Two workloads, because one number would be misleading
#
#   matched     both tools hold to the lints that both implement, which is
#               30 of them. This is the honest head to head. The set is
#               computed at run time, not written down here: larvae's half
#               comes out of its own schema, and selene's half comes from
#               SELENE_LINTS below, which the probe checks
#
#   defaults    each tool with what it enables out of the box. larvae runs
#               49 lints and selene runs 33, so the two sides do different
#               work. The row is here because it is what a user gets, and
#               the finding counts beside it say how much each side did
#
# Neither tool caches a lint run, so there is no cold and warm split here.
# Every run is the whole tree. `larvae process` has a cache and
# scripts/bench_darklua.sh measures it.
#
# Holding a lint off is not free on either side. selene drops from 29 ms to
# 21 ms on 1500 files when 15 lints go to "allow", so the matched row cannot
# be faked by leaving one side at its defaults.
#
# The one large file row is slow, and selene is why. Its cost per byte climbs
# with the size of the file: on this machine 124 KB takes 278 ms and 1 MB takes
# 18 s, so four times the bytes is sixty times the work. larvae stays linear
# over the same range. That row therefore takes BIG_RUNS samples, not RUNS.
#
# Usage  scripts/bench_selene.sh [file counts...]   defaults to 3000 5000
# Env    LARVAE=path SELENE=path RUNS=n BIG_RUNS=n
set -euo pipefail

SIZES=("${@:-3000 5000}")
[ $# -eq 0 ] && SIZES=(3000 5000)
RUNS="${RUNS:-7}"
# One 1 MB file costs selene about 18 s, so this row samples fewer times. The
# gap there is wide enough that more samples would not change the reading.
BIG_RUNS="${BIG_RUNS:-3}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LARVAE="${LARVAE:-$ROOT/target/release/larvae}"
SELENE="${SELENE:-selene}"

# Every lint selene 0.31 ships. The list is here because selene has no
# command that prints it, and it ignores a name it does not know rather than
# refusing it, so a typo would silently leave a lint running. `verify_lints`
# checks the list against what selene actually reports.
SELENE_LINTS=(
    almost_swapped bad_string_escape compare_nan constant_table_comparison
    deprecated divide_by_zero duplicate_keys empty_if empty_loop global_usage
    high_cyclomatic_complexity if_same_then_else ifs_same_cond
    incorrect_standard_library_use manual_table_clone mismatched_arg_count
    mixed_table multiple_statements must_use parenthese_conditions
    restricted_module_paths roblox_incorrect_color3_new_bounds
    roblox_incorrect_roact_usage roblox_manual_fromscale_or_fromoffset
    roblox_suspicious_udim2_new shadowing standard_library
    suspicious_reverse_loop type_check_inside_call unbalanced_assignments
    undefined_variable unscoped_variables unused_variable
)

# Both tools take the same standard library, so neither pays for globals the
# other does not have. "luau" ships inside selene, so this needs no network.
STD=luau

if [ ! -x "$LARVAE" ]; then
    echo "building larvae (release)..." >&2
    (cd "$ROOT" && cargo build --release)
fi
HAVE_SELENE=1
command -v "$SELENE" >/dev/null || {
    echo "note: selene not found, running larvae only (set SELENE= to compare)" >&2
    HAVE_SELENE=0
}

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# --- the lints the two tools share -------------------------------------------

# larvae's list comes from the schema it ships, so this cannot drift from the
# lints larvae actually has.
mapfile -t LARVAE_LINTS < <(
    python3 - "$ROOT/crates/larvae/larvae.schema.json" <<'PY'
import json, sys
schema = json.load(open(sys.argv[1]))
for name in sorted(schema["$defs"]["lint_rules"]["properties"]):
    print(name)
PY
)

MATCHED=()
for lint in "${LARVAE_LINTS[@]}"; do
    for other in "${SELENE_LINTS[@]}"; do
        [ "$lint" = "$other" ] && MATCHED+=("$lint") && break
    done
done

holds() { # holds <name> <list...>
    local want="$1"
    shift
    for have in "$@"; do [ "$have" = "$want" ] && return 0; done
    return 1
}

# A file that trips as many lints as one file can, so the probe below has
# something to compare. It is not part of any timed run.
probe_source() {
    cat <<'EOF'
local a, b = 1, 2
a, b = b, a
local dupe = { x = 1, x = 2 }
local escape = "\q"
if {} == {} then end
if true then end
while true do end
unscoped = 1
if a then b() elseif a then b() end
if a then b() else b() end
local mixed = { 1, 2, k = 3 }
local one = 1; local two = 2
if (a) then end
local missing = undefinedGlobalName
local never_read = 5
local zero = 1 / 0
return dupe, escape, mixed, one, two, missing, never_read, zero
EOF
}

# Reports the lints each tool names on the probe, and refuses to run when
# SELENE_LINTS has fallen behind the selene on this machine.
verify_lints() {
    local dir="$WORK/probe"
    mkdir -p "$dir/src"
    probe_source >"$dir/src/probe.luau"
    printf 'std = "%s"\n' "$STD" >"$dir/selene.toml"

    # Both tools exit non zero when they find something, and finding
    # something is the point of the probe, so neither exit code is an error.
    local sel_codes lar_codes drifted=()
    sel_codes=$(cd "$dir" && { "$SELENE" --display-style Json2 src 2>/dev/null || true; } |
        python3 -c "
import sys, json
for line in sys.stdin:
    try:
        d = json.loads(line)
    except ValueError:
        continue
    if d.get('type') == 'Diagnostic':
        print(d['code'])
" | sort -u || true)

    lar_codes=$({ "$LARVAE" lint --stdin <"$dir/src/probe.luau" 2>&1 || true; } |
        grep -oE '\([a-z_]+\)$' | tr -d '()' | sort -u || true)

    for code in $sel_codes; do
        holds "$code" "${SELENE_LINTS[@]}" || drifted+=("$code")
    done

    if [ ${#drifted[@]} -gt 0 ]; then
        echo "SELENE_LINTS is out of date, selene also reports: ${drifted[*]}" >&2
        echo "add them to the list at the top of this script, or the matched" >&2
        echo "row leaves them running on selene's side alone" >&2
        exit 1
    fi

    PROBE_SHARED=$(comm -12 <(echo "$sel_codes") <(echo "$lar_codes") | wc -l)
    PROBE_SELENE=$(echo "$sel_codes" | grep -c . || true)
    PROBE_LARVAE=$(echo "$lar_codes" | grep -c . || true)
}

# --- the tree ----------------------------------------------------------------

scaffold() { # scaffold <files> <body lines per file>
    python3 - "$1" "$2" <<'PY'
import os, sys
files, lines = int(sys.argv[1]), int(sys.argv[2])
dirs = max(1, files // 50)
os.makedirs("src", exist_ok=True)
body = "".join(f"    acc += t[{i}] * {i}\n" for i in range(lines))
n = 0
for d in range(dirs):
    os.makedirs(f"src/mod{d}", exist_ok=True)
    for i in range(50):
        if n >= files:
            break
        # Every tenth file carries something to report, so the run measures
        # the reporting path and not only a clean parse. Two findings are
        # lints both tools have, and one is larvae's alone, which is how the
        # counts below show that the matched configs really do hold the rest
        # off rather than silently running everything.
        #
        # The clean part of the file avoids what the two tools read
        # differently. `if a > 0 then acc += 1 end` on one line is such a
        # case: selene calls that multiple_statements and larvae does not,
        # because larvae compares statements within a block and the body
        # there is its own block. Two of those per file would have selene
        # formatting several thousand more diagnostics than larvae, and the
        # timings would then compare reporting volume, not analysis.
        faults = ""
        if i % 10 == 0:
            faults = (
                "local never_read = 1\n"
                "local shadowed = 1\n"
                "local shadowed = 2\n"
                "function M.concat(parts: { string }): string\n"
                "    local out = \"\"\n"
                "    for _, part in parts do out = out .. part end\n"
                "    return out\n"
                "end\n"
            )
        with open(f"src/mod{d}/sub{i}.luau", "w") as f:
            f.write(
                "--!strict\n"
                "-- a header comment, because real files have them\n"
                "local t = table.create(64, 1)\n"
                "local M = {}\n"
                f"function M.work(a: number, b: string?): number\n"
                "    local acc = a\n"
                + body
                + "    for j = 1, 8 do\n"
                "        acc += j\n"
                "    end\n"
                "    return acc + #(b or \"\")\n"
                "end\n"
                "function M.pick(flag: boolean)\n"
                "    return if flag then M.work(1, nil) else 0\n"
                "end\n"
                + faults
                + "return M\n"
            )
        n += 1
    if n >= files:
        break
PY
}

configs() { # configs, writes the four config files into the current tree
    printf 'std = "%s"\n' "$STD" >selene-default.toml
    {
        printf 'std = "%s"\n\n[lints]\n' "$STD"
        for lint in "${SELENE_LINTS[@]}"; do
            holds "$lint" "${MATCHED[@]}" || echo "$lint = \"allow\""
        done
    } >selene-matched.toml

    printf 'input = "src"\noutput = "dist"\n\n[lint]\nstd = "%s"\n' "$STD" >larvae.toml
    {
        cat larvae.toml
        echo
        echo "[lint.rules]"
        for lint in "${LARVAE_LINTS[@]}"; do
            holds "$lint" "${MATCHED[@]}" || echo "$lint = \"allow\""
        done
    } >larvae-matched.toml
}

# --- measuring ---------------------------------------------------------------

MEDIAN=0
SAMPLES=0
bench() { # bench <cmd...>, sets MEDIAN in ms
    "$@" >/dev/null 2>&1 || true
    local times=() start end
    for _ in $(seq "$SAMPLES"); do
        start=$(date +%s%N)
        "$@" >/dev/null 2>&1 || true
        end=$(date +%s%N)
        times+=($(((end - start) / 1000000)))
    done
    mapfile -t times < <(printf '%s\n' "${times[@]}" | sort -n)
    MEDIAN=${times[$((SAMPLES / 2))]}
}

ratio() { # ratio <slow> <fast>
    if [ "$2" -le 0 ] || [ "$1" -le 0 ]; then
        echo "-"
        return
    fi
    local r=$(($1 * 10 / $2))
    echo "$((r / 10)).$((r % 10))x"
}

# selene exits non zero when it reports a warning and larvae does not, and
# `grep -c` exits non zero when it counts none. Under `pipefail` both of those
# fail the pipeline, and `n=$(...) || n=0` would then throw the count away and
# report zero. So pipefail comes off for the count itself, and the exit codes
# are read as what they are: not errors.
count_lines() { # count_lines <pattern> — reads stdin, always prints a number
    local n
    n=$( (
        set +o pipefail
        grep -cE "$1"
    ) || true)
    echo "${n:-0}"
}

larvae_findings() { # larvae_findings <config>
    { "$LARVAE" lint --config "$1" 2>&1 || true; } | count_lines '^(warning|error)'
}

selene_findings() { # selene_findings <config>
    { "$SELENE" --config "$1" --display-style Json2 src 2>/dev/null || true; } |
        count_lines '"type":"Diagnostic"'
}

ROWS=()
COUNTS=()

run_scenarios() { # run_scenarios <label>
    local label="$1"

    bench "$LARVAE" lint --config larvae-matched.toml
    local lar_matched=$MEDIAN
    bench "$LARVAE" lint --config larvae.toml
    local lar_default=$MEDIAN

    if [ "$HAVE_SELENE" = 0 ]; then
        ROWS+=("$label|matched|${lar_matched} ms|-|-")
        ROWS+=("$label|defaults|${lar_default} ms|-|-")
        return
    fi

    bench "$SELENE" --config selene-matched.toml src
    local sel_matched=$MEDIAN
    bench "$SELENE" --config selene-default.toml src
    local sel_default=$MEDIAN

    ROWS+=("$label|matched|${lar_matched} ms|${sel_matched} ms|$(ratio "$sel_matched" "$lar_matched")")
    ROWS+=("$label|defaults|${lar_default} ms|${sel_default} ms|$(ratio "$sel_default" "$lar_default")")

    COUNTS+=("$label|$(larvae_findings larvae.toml)|$(larvae_findings larvae-matched.toml)|$(selene_findings selene-default.toml)|$(selene_findings selene-matched.toml)")
}

# --- run ---------------------------------------------------------------------

if [ "$HAVE_SELENE" = 1 ]; then
    verify_lints
    echo "probe: selene names $PROBE_SELENE lints, larvae names $PROBE_LARVAE, $PROBE_SHARED the same" >&2
fi
echo "matched set is ${#MATCHED[@]} lints of larvae's ${#LARVAE_LINTS[@]} and selene's ${#SELENE_LINTS[@]}" >&2

for size in "${SIZES[@]}"; do
    dir="$WORK/p$size"
    mkdir -p "$dir"
    cd "$dir"
    echo "benchmarking $size files, $RUNS runs each..." >&2
    SAMPLES="$RUNS"
    scaffold "$size" 20
    configs
    run_scenarios "$size"
done

echo "benchmarking one large file..." >&2
dir="$WORK/big"
mkdir -p "$dir"
cd "$dir"
scaffold 1 5
configs
python3 - <<'PY'
# one module of roughly a megabyte, the single file throughput case
lines = ['--!strict', 'local M = {}']
for i in range(6000):
    lines.append(f'function M.fn{i}(a: number, b: string?): number')
    lines.append(f'    local t = {{ id = {i}, name = "item{i}", flag = a > {i} }}')
    lines.append(f'    return if t.flag then a + {i} else #(b or "") * 2')
    lines.append('end')
lines.append('return M')
open("src/mod0/sub0.luau", "w").write("\n".join(lines) + "\n")
PY
BIG_BYTES=$(wc -c <src/mod0/sub0.luau)
echo "  big module is $BIG_BYTES bytes, $BIG_RUNS runs (selene is slow here)" >&2
SAMPLES="$BIG_RUNS"
run_scenarios "1 big"

# --- report ------------------------------------------------------------------

cd "$ROOT"
echo
echo "machine: $(nproc) cores, $RUNS runs per cell, median reported"
[ "$HAVE_SELENE" = 1 ] && echo "versions: $("$LARVAE" --version), $("$SELENE" --version)"
echo
printf "| %-6s | %-9s | %-9s | %-9s | %-7s |\n" "Files" "workload" "larvae" "selene" "speedup"
printf "|%s|%s|%s|%s|%s|\n" "-------:" ":----------" "----------:" "----------:" "--------:"
for row in "${ROWS[@]}"; do
    IFS='|' read -r f w l s r <<<"$row"
    printf "| %6s | %-9s | %9s | %9s | %7s |\n" "$f" "$w" "$l" "$s" "$r"
done

echo
echo "matched   both hold to the ${#MATCHED[@]} lints that both implement"
echo "defaults  larvae runs ${#LARVAE_LINTS[@]} lints, selene runs ${#SELENE_LINTS[@]}, so the two sides"
echo "          do different work. The counts below say how much"

if [ "$HAVE_SELENE" = 1 ] && [ ${#COUNTS[@]} -gt 0 ]; then
    echo
    echo "findings on the same tree, so the configs above are doing what they say"
    echo
    printf "| %-6s | %-11s | %-11s | %-11s | %-11s |\n" \
        "Files" "larvae def" "larvae mat" "selene def" "selene mat"
    printf "|%s|%s|%s|%s|%s|\n" \
        "-------:" "------------:" "------------:" "------------:" "------------:"
    for row in "${COUNTS[@]}"; do
        IFS='|' read -r f ld lm sd sm <<<"$row"
        printf "| %6s | %11s | %11s | %11s | %11s |\n" "$f" "$ld" "$lm" "$sd" "$sm"
    done
    echo
    echo "larvae reports more at its defaults than under the matched set, which"
    echo "is the 19 lints selene does not have. The two matched columns count"
    echo "the same lints over the same files"
fi

echo
echo
echo "the 1 big row is one module of about a megabyte. selene's cost per byte"
echo "climbs with the size of a file and larvae's does not, so that row reads"
echo "far wider than the file tree rows. It is one file, not a tree, and no"
echo "project is only one file: read it as a parser and analysis result, not"
echo "as the number a build sees"
echo
echo "larvae reads selene.toml directly, so a project can point larvae at the"
echo "config it already has. This script writes separate files only to hold the"
echo "two tools to the same set of lints"
