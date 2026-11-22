#!/usr/bin/env bash
# Checks packaging/open-prism against a fake package whose open-prism-api prints the arguments
# it gets. Run from anywhere: packaging/test-launcher.sh
set -euo pipefail

source_dir=$(cd -- "$(dirname -- "$0")" && pwd -P)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
root="$work/open prism %pkg"
mkdir -p "$root/bin" "$root/share"
cp "$source_dir/open-prism" "$root/open-prism"
cp "$source_dir/open-prism.svg" "$root/share/open-prism.svg" 2>/dev/null || touch "$root/share/open-prism.svg"
printf '#!/bin/sh\nprintf "%%s|" "$@"\n' > "$root/bin/open-prism-api"
chmod 755 "$root/open-prism" "$root/bin/open-prism-api"

failures=0
check() {
  local name=$1 expected=$2 actual=$3
  if [ "$actual" = "$expected" ]; then
    echo "ok   $name"
  else
    echo "FAIL $name"
    echo "     expected: $expected"
    echo "     actual:   $actual"
    failures=$((failures + 1))
  fi
}

serve="serve|--web|$root/web|--node|$root/node/bin/node|"
run() { (cd / && env -u OPENPRISM_NO_BROWSER "$root/open-prism" "$@"); }

check "no arguments" "${serve}--open|" "$(run)"
check "OPENPRISM_NO_BROWSER=1" "$serve" "$(cd / && OPENPRISM_NO_BROWSER=1 "$root/open-prism")"
check "options go to serve" "${serve}--open|--port|8799|" "$(run --port 8799)"
check "serve typed by hand" "${serve}--open|--web-port|3799|" "$(run serve --web-port 3799)"
check "print-token unchanged" "--data-dir|X|print-token|" "$(run --data-dir X print-token)"
check "--version unchanged" "--version|" "$(run --version)"
check "user commands unchanged" "--mode|server|user|reset-password|lan|" \
  "$(run --mode server user reset-password lan)"
check "help unchanged" "help|" "$(run help)"

# The application menu, in a throwaway data home.
data_home="$work/data home"
export XDG_DATA_HOME="$data_home"
run install-desktop > /dev/null
entry="$data_home/applications/open-prism.desktop"
icon="$data_home/icons/hicolor/scalable/apps/open-prism.svg"
check "menu entry written" "yes" "$([ -f "$entry" ] && echo yes || echo no)"
check "icon copied" "yes" "$([ -f "$icon" ] && echo yes || echo no)"
check "Exec is the absolute launcher, % doubled" \
  "Exec=\"$work/open prism %%pkg/open-prism\"" "$(grep '^Exec=' "$entry")"
check "runs in a terminal" "Terminal=true" "$(grep '^Terminal=' "$entry")"
run uninstall-desktop > /dev/null
check "uninstall removes both" "no no" \
  "$([ -e "$entry" ] && echo yes || echo no) $([ -e "$icon" ] && echo yes || echo no)"

# A path the menu entry cannot quote is refused.
odd="$work/odd\$dir"
mkdir -p "$odd/share"
cp "$root/open-prism" "$odd/open-prism"
touch "$odd/share/open-prism.svg"
if (cd / && "$odd/open-prism" install-desktop) > /dev/null 2>&1; then
  check "path with \$ refused" "refused" "installed"
else
  check "path with \$ refused" "refused" "refused"
fi

if [ "$failures" -gt 0 ]; then
  echo "$failures check(s) failed"
  exit 1
fi
echo "all launcher checks passed"
