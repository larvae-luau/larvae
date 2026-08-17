#!/usr/bin/env bash
# Compare larvae's parser against the real Luau parser, file by file
#
# larvae has a parser of its own, so "does it read Luau the way Luau does" is
# a question that only a comparison answers. luau-lsp embeds the real parser
# and prints `SyntaxError:` apart from `TypeError:`, which is the whole trick:
# the type errors are not larvae's business and the syntax errors are.
#
# The verdicts in crates/larvae/tests/fixtures/parser are recorded, so CI runs
# the comparison without Luau. This script is how the recording gets made and
# checked again later.
#
#   scripts/parser_oracle.sh                    # check the recorded fixtures
#   scripts/parser_oracle.sh path/to/corpus     # sweep a tree of your own
#   scripts/parser_oracle.sh --list files.txt   # sweep a list of paths
#
# Env  LARVAE=path LUAU_LSP=path
#
# A sweep prints two lists. `larvae refuses` is the one that costs a user
# something: correct Luau that no larvae command will read. `larvae parses` is
# the softer direction, and the fixtures under `lenient` record the ones that
# are known and harmless.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LARVAE="${LARVAE:-$ROOT/target/release/larvae}"
LUAU_LSP="${LUAU_LSP:-luau-lsp}"
FIXTURES="$ROOT/crates/larvae/tests/fixtures/parser"

if [ ! -x "$LARVAE" ]; then
    echo "building larvae (release)..." >&2
    (cd "$ROOT" && cargo build --release)
fi

command -v "$LUAU_LSP" >/dev/null || {
    echo "luau-lsp not found. It carries the real parser, so the comparison" >&2
    echo "cannot run without it. Set LUAU_LSP=path, or install it." >&2
    exit 1
}

# Reports the verdict of the real Luau parser: ok, or reject.
luau_says() { # luau_says <file>
    case "$("$LUAU_LSP" analyze --platform=standard "$1" 2>&1 || true)" in
        *SyntaxError:*) echo reject ;;

        *) echo ok ;;
    esac
}

# Reports the verdict of larvae's parser.
larvae_says() { # larvae_says <file>
    if "$LARVAE" fmt --stdin <"$1" >/dev/null 2>&1; then
        echo ok
    else
        echo reject
    fi
}

# --- checking the recorded fixtures ------------------------------------------

check_fixtures() {
    local wrong=0

    for group in accept reject lenient; do
        local want
        case "$group" in
            accept) want=ok ;;
            *) want=reject ;;
        esac

        for file in "$FIXTURES/$group"/*.luau; do
            [ -e "$file" ] || continue

            local got
            got=$(luau_says "$file")

            if [ "$got" != "$want" ]; then
                echo "RECORDING IS STALE  $group/$(basename "$file")"
                echo "    recorded as \"$want\" for Luau, this Luau says \"$got\""
                wrong=$((wrong + 1))
            fi
        done
    done

    if [ "$wrong" -gt 0 ]; then
        echo
        echo "$wrong fixture(s) no longer match this Luau. Move the file to the"
        echo "directory that matches, and say so in the commit."

        return 1
    fi

    echo "every recorded verdict matches $("$LUAU_LSP" --version 2>&1 | head -1)"
}

# --- sweeping a corpus -------------------------------------------------------

sweep() { # sweep <list of paths on stdin>
    local n=0 agree=0 strict=0 lenient=0
    local strict_list=() lenient_list=()

    while read -r file; do
        [ -f "$file" ] || continue
        n=$((n + 1))

        local lar luau
        lar=$(larvae_says "$file")
        luau=$(luau_says "$file")

        if [ "$lar" = "$luau" ]; then
            agree=$((agree + 1))
        elif [ "$lar" = reject ]; then
            strict=$((strict + 1))
            strict_list+=("$file")
        else
            lenient=$((lenient + 1))
            lenient_list+=("$file")
        fi
    done

    echo
    echo "$n files, $agree agree"
    echo

    echo "larvae refuses what Luau parses: $strict"
    for file in ${strict_list[@]+"${strict_list[@]}"}; do
        echo "    $file"
    done

    echo
    echo "larvae parses what Luau refuses: $lenient"
    for file in ${lenient_list[@]+"${lenient_list[@]}"}; do
        echo "    $file"
    done

    echo
    echo "The first list is the one to act on. A file there is correct Luau that"
    echo "no larvae command reads. Add it under fixtures/parser/accept with the"
    echo "fix. The second list is softer; record a case under fixtures/parser/"
    echo "lenient when it is one larvae means to keep."

    [ "$strict" -eq 0 ]
}

# --- run ---------------------------------------------------------------------

case "${1:-}" in
    "")
        check_fixtures
        ;;

    --list)
        [ -n "${2:-}" ] || {
            echo "--list needs a file of paths" >&2
            exit 2
        }
        sweep <"$2"
        ;;

    *)
        find "$1" -type f \( -name '*.luau' -o -name '*.lua' \) | sweep
        ;;
esac
