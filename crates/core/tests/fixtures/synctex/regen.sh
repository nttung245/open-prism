#!/usr/bin/env bash
# Regenerates the SyncTeX fixtures used by crates/core/tests/synctex.rs.
#
# For each case directory (or only the ones named on the command line) this
#   1. copies the case's .tex sources to a scratch project directory,
#   2. compiles them with latexmk inside the TeX Live Docker image, exactly like the
#      compile service does (project mounted at /data/projects/fixture, build directory
#      at /data/build/fixture, `-synctex=1`), with the engine named by the
#      `% !TEX program = pdflatex|xelatex|lualatex` comment on the first line of main.tex,
#   3. runs oracle.py in the same container, which asks the official `synctex` CLI a
#      fixed set of forward (`synctex view`) and inverse (`synctex edit`) queries,
#   4. copies main.synctex.gz and expected.json back into the case directory.
#
# Nothing but Docker is needed on the host; no PDF or other build output is kept.
#
#   crates/core/tests/fixtures/synctex/regen.sh                 # every case
#   crates/core/tests/fixtures/synctex/regen.sh twocolumn       # one case
#   TEXLIVE_IMAGE=texlive/texlive:latest-small regen.sh ...     # another image
#   ORACLE_COUNTS="100000 2000" SYNCTEX_FIXTURE_OUT=/tmp/wide regen.sh
#       # every source line and 2000 points per case, written elsewhere; compare with
#       # SYNCTEX_FIXTURES=/tmp/wide cargo test -p openprism-core --test synctex -- --nocapture
#
# Scratch files go to $TMPDIR (default /tmp) and are removed on exit.
set -euo pipefail

image="${TEXLIVE_IMAGE:-texlive/texlive:latest}"
here="$(cd "$(dirname "$0")" && pwd)"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/synctex-fixtures.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT

if [ "$#" -gt 0 ]; then
  cases=("$@")
else
  cases=()
  for dir in "$here"/*/; do
    [ -f "$dir/main.tex" ] && cases+=("$(basename "$dir")")
  done
fi

for case in "${cases[@]}"; do
  src="$here/$case"
  program="$(sed -n '1s/^% !TEX program = \([a-z]*\).*/\1/p' "$src/main.tex")"
  case "$program" in
    pdflatex) engine=-pdf ;;
    xelatex) engine=-xelatex ;;
    lualatex) engine=-lualatex ;;
    *) echo "$case: first line of main.tex must be '% !TEX program = <engine>'" >&2; exit 1 ;;
  esac

  project="$scratch/$case/project"
  build="$scratch/$case/build"
  mkdir -p "$project" "$build"
  (cd "$src" && find . -name '*.tex' -exec cp --parents {} "$project" \;)

  echo "==> $case ($program)"
  docker run --rm --network none -e HOME=/tmp --user "$(id -u):$(id -g)" \
    -v "$project:/data/projects/fixture" -v "$build:/data/build/fixture" \
    -v "$here/oracle.py:/oracle/oracle.py:ro" \
    -w /data/projects/fixture "$image" sh -c "
      latexmk $engine -f -interaction=nonstopmode -file-line-error -synctex=1 \
        -outdir=/data/build/fixture main.tex >/data/build/fixture/latexmk.out 2>&1 \
        || { tail -n 40 /data/build/fixture/latexmk.out; exit 1; }
      python3 -I /oracle/oracle.py '$case' /data/build/fixture/expected.json ${ORACLE_COUNTS:-}
    "

  out="${SYNCTEX_FIXTURE_OUT:-$here}/$case"
  mkdir -p "$out"
  cp "$build/main.synctex.gz" "$build/expected.json" "$out/"
done
