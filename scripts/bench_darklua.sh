#!/usr/bin/env bash
# Benchmark larvae against darklua on synthetic Rojo shaped projects
#
# Three workloads, because one number would be misleading
#
#   parse only   darklua runs no rules with the retain_lines generator, which
#                is the least work it can be asked to do. larvae still
#                resolves and rewrites every require in this row, so it is
#                doing strictly more, the row is here as darklua's floor
#
#   same rules   both tools run the same ten rules, retain_lines on both
#                sides. This is the honest head to head
#
#   darklua      darklua's own default config, which is its default rule
#   default      stack plus the dense generator. larvae has both now,
#                generator = "dense" and rename_variables, but this row
#                still runs darklua alone so the history of the numbers
#                stays comparable. Making it a head to head is future work
#
# Scenarios per size
#   cold      first build, nothing cached
#   warm      nothing changed since the last build
#   one edit  a single file touched, everything else cached
#   check     validation only, this one parses every file
#
# Usage  scripts/bench.sh [file counts...]   defaults to 3000 5000
# Env    LARVAE=path DARKLUA=path RUNS=n
set -euo pipefail

SIZES=("${@:-3000 5000}")
[ $# -eq 0 ] && SIZES=(3000 5000)
RUNS="${RUNS:-7}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LARVAE="${LARVAE:-$ROOT/target/release/larvae}"
DARKLUA="${DARKLUA:-darklua}"

# rules both tools implement under the same name, this is the matched workload
MATCHED_RULES=(
    compute_expression
    convert_index_to_field
    filter_after_early_return
    remove_comments
    remove_empty_do
    remove_function_call_parens
    remove_method_definition
    remove_nil_declaration
    remove_unused_if_branch
    remove_unused_while
)

if [ ! -x "$LARVAE" ]; then
    echo "building larvae (release)..." >&2
    (cd "$ROOT" && cargo build --release)
fi
HAVE_DARKLUA=1
command -v "$DARKLUA" >/dev/null || {
    echo "note: darklua not found, running larvae only (set DARKLUA= to compare)" >&2
    HAVE_DARKLUA=0
}

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

scaffold() { # scaffold <files> <body lines per file>
    python3 - "$1" "$2" <<'PY'
import os, sys
files, lines = int(sys.argv[1]), int(sys.argv[2])
dirs = max(1, files // 50)
os.makedirs("src", exist_ok=True)
body = "".join(f"    t[{i}] = i * 2\n" for i in range(lines))
n = 0
for d in range(dirs):
    os.makedirs(f"src/mod{d}", exist_ok=True)
    with open(f"src/mod{d}/init.luau", "w") as f:
        f.write("return {\n" + "".join(f'  m{i} = require("@self/sub{i}"),\n' for i in range(5)) + "}\n")
    n += 1
    i = 0
    while n < files and i < 49:
        with open(f"src/mod{d}/sub{i}.luau", "w") as f:
            dep = f'local dep = require("./sub{(i + 1) % 49}")\n' if i != 48 else "local dep = nil\n"
            f.write("--!strict\n-- header comment\n" + dep
                    + 'local pkg = require("@pkg/signal")\n'
                    + "local t = {}\nlocal function fill()\n" + body + "end\nfill()\nreturn { t, dep, pkg }\n")
        n += 1
        i += 1
    if n >= files:
        break
os.makedirs("Packages", exist_ok=True)
open("Packages/signal.luau", "w").write("return {}\n")
PY
    cat > default.project.json <<'EOF'
{
  "name": "bench",
  "tree": {
    "$className": "DataModel",
    "ReplicatedStorage": {
      "app": { "$path": "src" },
      "Packages": { "$path": "Packages" }
    }
  }
}
EOF
    cat > larvae.toml <<'EOF'
[aliases]
pkg = "@game/ReplicatedStorage/Packages"
EOF
    {
        cat larvae.toml
        echo
        echo "[rules]"
        for r in "${MATCHED_RULES[@]}"; do echo "$r = true"; done
    } > cold-rules.toml

    echo '{ "rules": [], "generator": "retain_lines" }' > dark-bare.json
    python3 - "${MATCHED_RULES[@]}" <<'PY'
import json, sys
json.dump({"rules": sys.argv[1:], "generator": "retain_lines"}, open("dark-rules.json", "w"))
PY
    echo '{}' > dark-default.json
}

MEDIAN=0
FASTEST=0
bench() { # bench <setup> <cmd...>, sets MEDIAN and FASTEST in ms
    local setup="$1"
    shift
    "$setup"
    "$@" >/dev/null 2>&1 || {
        echo "command failed:" "$@" >&2
        exit 1
    }
    local times=() start end
    for _ in $(seq "$RUNS"); do
        "$setup"
        start=$(date +%s%N)
        "$@" >/dev/null 2>&1
        end=$(date +%s%N)
        times+=($(((end - start) / 1000000)))
    done
    mapfile -t times < <(printf '%s\n' "${times[@]}" | sort -n)
    MEDIAN=${times[$((RUNS / 2))]}
    FASTEST=${times[0]}
}

ratio() { # ratio <slow> <fast>
    if [ "$2" -le 0 ] || [ "$1" -le 0 ]; then
        echo "-"
        return
    fi
    local r=$(($1 * 10 / $2))
    echo "$((r / 10)).$((r % 10))x"
}

drop_cache() { rm -rf .larvae dist dist-darklua; }
keep_cache() { :; }
touch_one() {
    # no pipe to head here, it would SIGPIPE find and pipefail would abort
    local f
    f="$(find src -name '*.luau' -print -quit)"
    [ -n "$f" ] && touch "$f"
    return 0
}

CACHE_ROWS=()
HEAD_ROWS=()

run_scenarios() { # run_scenarios <label>
    local label="$1"

    bench drop_cache "$LARVAE" process
    local cold=$MEDIAN
    bench keep_cache "$LARVAE" process
    local warm=$MEDIAN
    bench touch_one "$LARVAE" process
    local one=$MEDIAN
    bench keep_cache "$LARVAE" check
    local check=$MEDIAN
    CACHE_ROWS+=("$label|${cold} ms|${warm} ms|${one} ms|${check} ms")

    if [ "$HAVE_DARKLUA" = 0 ]; then
        return
    fi

    # parse only, darklua's floor
    bench drop_cache "$DARKLUA" process --config dark-bare.json src dist-darklua
    HEAD_ROWS+=("$label|parse only|${cold} ms|${MEDIAN} ms|$(ratio "$MEDIAN" "$cold")")

    # same ten rules on both sides
    bench drop_cache "$LARVAE" process --config cold-rules.toml
    local cold_rules=$MEDIAN
    bench drop_cache "$DARKLUA" process --config dark-rules.json src dist-darklua
    HEAD_ROWS+=("$label|same rules|${cold_rules} ms|${MEDIAN} ms|$(ratio "$MEDIAN" "$cold_rules")")

    # darklua's own default, which larvae cannot match yet
    bench drop_cache "$DARKLUA" process --config dark-default.json src dist-darklua
    HEAD_ROWS+=("$label|darklua default|n/a|${MEDIAN} ms|-")
}

for size in "${SIZES[@]}"; do
    dir="$WORK/p$size"
    mkdir -p "$dir"
    cd "$dir"
    echo "benchmarking $size files, $RUNS runs each..." >&2
    scaffold "$size" 20
    run_scenarios "$size"
done

echo "benchmarking one large file..." >&2
dir="$WORK/big"
mkdir -p "$dir"
cd "$dir"
scaffold 2 5
python3 - <<'PY'
# one module of roughly a megabyte, the single file parser throughput case
lines = ['--!strict', 'local pkg = require("@pkg/signal")', 'local M = {}']
for i in range(20000):
    lines.append(f'function M.fn{i}(a: number, b: string?): number')
    lines.append(f'    local t = {{ id = {i}, name = "item{i}", flag = a > {i} }}')
    lines.append(f'    return if t.flag then a + {i} else #(b or "") * 2')
    lines.append('end')
lines.append('return { M, pkg }')
open("src/mod0/sub0.luau", "w").write("\n".join(lines) + "\n")
PY
BIG_BYTES=$(wc -c < src/mod0/sub0.luau)
echo "  big module is $BIG_BYTES bytes" >&2
run_scenarios "1 big"

# --- report ------------------------------------------------------------------
cd "$ROOT"
echo
echo "machine: $(nproc) cores, $RUNS runs per cell, median reported"
[ "$HAVE_DARKLUA" = 1 ] && echo "versions: $("$LARVAE" --version), $("$DARKLUA" --version)"
echo
echo "larvae incremental build, darklua has no cache so these are ours alone"
echo
printf "| %-6s | %-8s | %-8s | %-8s | %-8s |\n" "Files" "cold" "warm" "one edit" "check"
printf "|%s|%s|%s|%s|%s|\n" "-------:" "---------:" "---------:" "---------:" "---------:"
for row in "${CACHE_ROWS[@]}"; do
    IFS='|' read -r f c w o ch <<<"$row"
    printf "| %6s | %8s | %8s | %8s | %8s |\n" "$f" "$c" "$w" "$o" "$ch"
done

if [ "$HAVE_DARKLUA" = 1 ]; then
    echo
    echo "head to head, both cold, same input tree"
    echo
    printf "| %-6s | %-15s | %-9s | %-9s | %-7s |\n" \
        "Files" "workload" "larvae" "darklua" "speedup"
    printf "|%s|%s|%s|%s|%s|\n" "-------:" ":----------------" "----------:" "----------:" "--------:"
    for row in "${HEAD_ROWS[@]}"; do
        IFS='|' read -r f w c d s <<<"$row"
        printf "| %6s | %-15s | %9s | %9s | %7s |\n" "$f" "$w" "$c" "$d" "$s"
    done
    echo
    echo "parse only    darklua runs no rules, larvae still rewrites every require"
    echo "same rules    both run ${#MATCHED_RULES[@]} matching rules with retain_lines"
    echo "darklua default   darklua's default stack plus its dense generator, shown"
    echo "                  alone so the history of the numbers stays comparable"
    echo
    echo "darklua can convert requires with a rojo sourcemap, that is not enabled here"
    echo "because it needs a separate rojo run, so the require work is larvae only"
fi
