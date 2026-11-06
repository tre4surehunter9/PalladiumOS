# Copyright (c) 2026 tre4surehunter9
# Checks that the dash port works on PalladiumOS: POSIX shell features that
# depend on the OS (fork/exec, pipes, redirections, signals, waiting).
# Run by tests/usermode.rs with dash; prints "dash checks passed".
set -u
fail() { echo "FAILED: $*"; exit 1; }
T=${TMPDIR:-/tmp}/dash-check.$$
mkdir "$T" || fail mkdir

# expansions
x=/a/b/c.txt
[ "${x##*/}" = c.txt ] || fail basename
[ "${x%.*}" = /a/b/c ] || fail suffix
[ "${#x}" = 10 ] || fail length
[ "${unset_var:-dflt}" = dflt ] || fail default
[ $((7 * 6 + 1)) = 43 ] && [ $((17 % 5)) = 2 ] || fail arith
set -- one "two words" three
[ $# = 3 ] && [ "$2" = "two words" ] || fail params
case "hello.c" in *.c) ;; *) fail case ;; esac

# functions, subshells, command substitution
add() { echo $(($1 + $2)); }
[ "$(add 2 3)" = 5 ] || fail function
v=outer; (v=inner); [ $v = outer ] || fail subshell
[ "$(echo a; echo b)" = "a
b" ] || fail cmdsub

# pipes and redirections
[ "$(printf '3\n1\n2\n' | sort | head -n 1)" = 1 ] || fail pipe
echo first > "$T/f"; echo second >> "$T/f"
[ "$(wc -l < "$T/f")" -eq 2 ] || fail redirect
ls /no/such 2> "$T/err" && fail ls
grep -q "No such file" "$T/err" || fail stderr
exec 3> "$T/fd3"; echo via-fd3 >&3; exec 3>&-
[ "$(cat "$T/fd3")" = via-fd3 ] || fail exec-fd
cat > "$T/here" <<END
line $x
END
[ "$(cat "$T/here")" = "line /a/b/c.txt" ] || fail heredoc
printf '%s\n' a b c | while read -r l; do echo "<$l>"; done > "$T/loop"
[ "$(cat "$T/loop")" = "<a>
<b>
<c>" ] || fail read-loop

# processes, exit statuses, waiting
sh -c 'exit 7'; [ $? = 7 ] || fail status
false | true || fail pipe-status
! false || fail negate
sleep 1 & pid=$!
kill -0 $pid || fail kill-0
wait $pid || fail wait
(exit 3) & wait $!; [ $? = 3 ] || fail wait-status
sleep 10 & k=$!
kill $k; wait $k; [ $? = 143 ] || fail killed-status
command -v ls > /dev/null || fail command-v

# signals and traps
trap 'got=usr1' USR1
kill -USR1 $$
[ "${got:-}" = usr1 ] || fail trap
r=$(sh -c 'trap "echo bye" EXIT; echo hi')
[ "$r" = "hi
bye" ] || fail exit-trap

# getopts, umask, test
OPTIND=1
while getopts "ab:" o -a -b val; do case $o in a) A=1 ;; b) B=$OPTARG ;; esac; done
[ "$A$B" = 1val ] || fail getopts
umask 077; : > "$T/private"; umask 022
case "$(ls -l "$T/private")" in -rw-------*) ;; *) fail umask ;; esac
[ -d "$T" ] && [ -f "$T/f" ] && [ ! -e "$T/none" ] || fail test

rm -r "$T" || fail cleanup
echo "dash checks passed"
