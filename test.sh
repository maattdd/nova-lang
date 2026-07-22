#!/usr/bin/env bash
# Compile and run every example through the full nova → C++ → binary pipeline.
# Usage: ./test.sh          (uses clang++; override with CXX=g++ ./test.sh)
#
# Pass/fail comes from the examples themselves: they assert with std/test and
# exit nonzero on a failed assertion. This script only adds two guards:
#   - an example importing std/test must actually print "ok" lines, so a
#     miscompilation that silently drops the assertions cannot pass;
#   - examples whose stdout IS the feature under test (@debug output) keep an
#     exact expected-output pin, since Nova can't capture its own output yet.
set -u
cd "$(dirname "$0")"

NOVA=./target/debug/nova
CXX=${CXX:-clang++}
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

cargo build -q 2>"$TMP/build.log" || { cat "$TMP/build.log"; exit 1; }

# Examples that must FAIL nova compilation (error-reporting demos)
EXPECT_COMPILE_ERROR="test_error"
# Examples needing the GC runtime prepended
NEEDS_STANDALONE="gc_refs"

# Exact stdout, only where the output itself is the tested feature.
declare -A EXPECT_OUT=(
    [matrix]=$'Dense {\n  sz: 42\n}\nDense {\n  sz: 5\n}\nok size via vtable\nok dot picks inner(Matrix, HotVector)'
    [test_debug]=$'Point {\n  x: 3\n  y: 2\n}'
    [test_nest]=$'Rect {\n  top_left: Point {\n    x: 0\n    y: 0\n  }\n  bottom_right: Point {\n    x: 10\n    y: 10\n  }\n}\nok nested field access'
)

pass=0; fail=0
for f in examples/*.nv; do
    n=$(basename "$f" .nv)
    flags=""
    [[ " $NEEDS_STANDALONE " == *" $n "* ]] && flags="--standalone"

    if ! $NOVA "$f" $flags -o "$TMP/$n.cpp" 2>"$TMP/$n.err"; then
        if [[ " $EXPECT_COMPILE_ERROR " == *" $n "* ]]; then
            echo "PASS $n (compile error, as expected)"; pass=$((pass+1))
        else
            echo "FAIL $n (nova): $(head -1 "$TMP/$n.err" | sed 's/\x1b\[[0-9;]*m//g')"; fail=$((fail+1))
        fi
        continue
    fi
    if [[ " $EXPECT_COMPILE_ERROR " == *" $n "* ]]; then
        echo "FAIL $n: expected a compile error, but nova accepted it"; fail=$((fail+1))
        continue
    fi

    if ! $CXX -std=c++20 -w -x c++ "$TMP/$n.cpp" -o "$TMP/$n" 2>"$TMP/$n.err"; then
        echo "FAIL $n (c++): $(grep -m1 error "$TMP/$n.err")"; fail=$((fail+1))
        continue
    fi

    out=$("$TMP/$n" 2>&1); rc=$?
    if [ "$rc" -ne 0 ]; then
        echo "FAIL $n: exit $rc"
        echo "$out" | sed 's/^/  /'
        fail=$((fail+1))
        continue
    fi
    # Assertions must have actually run
    if grep -q 'std/test' "$f" && ! grep -q '^ok ' <<<"$out"; then
        echo "FAIL $n: imports std/test but printed no 'ok' lines"; fail=$((fail+1))
        continue
    fi
    if [ -n "${EXPECT_OUT[$n]:-}" ] && [ "$out" != "${EXPECT_OUT[$n]}" ]; then
        echo "FAIL $n: output mismatch"
        echo "  want: ${EXPECT_OUT[$n]}"
        echo "  got:  $out"
        fail=$((fail+1))
        continue
    fi
    asserts=$(grep -c '^ok ' <<<"$out" || true)
    suffix=""
    [ "$asserts" -gt 0 ] && suffix=" ($asserts asserts)"
    echo "PASS $n$suffix"
    pass=$((pass+1))
done

echo "────"
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
