"""Records what the official `synctex` CLI answers for a compiled fixture.

Runs INSIDE the TeX Live container (see regen.sh), after latexmk, with the project at
/data/projects/fixture and the build at /data/build/fixture. Writes `expected.json`:

    forward: [{file, line, results: [{page, x, y, h, v, width, height}, ...]}]
    inverse: [{page, x, y, results: [{file, line, column}, ...]}]

Forward results are the CLI records verbatim (big points, origin top-left, `v` is the
bottom of the box and `height` includes the depth). Inverse points are in big points from
the top-left corner of the page. File paths are lexically normalised (no `./`).

Queries are chosen deterministically (seeded by the case name), spread across the pages
and the project's source files.

usage: python3 oracle.py CASE OUT.json [FORWARD_COUNT] [INVERSE_COUNT]
"""

import gzip
import json
import os
import random
import re
import subprocess
import sys

PROJECT = "/data/projects/fixture"
PDF = "/data/build/fixture/main.pdf"
SYNCTEX = "/data/build/fixture/main.synctex.gz"
# Every fixture uses a4paper.
PAGE_WIDTH, PAGE_HEIGHT = 595.28, 841.89
UNIT = 1 / 65781.76  # scaled points to big points (Unit:1, Magnification:1000)


def normalise(path):
    return os.path.normpath(path)


def read_synctex():
    """Input names by tag, and the hboxes `(h, v, w, height, depth)` of each page in bp."""
    inputs, pages, page = {}, {}, None
    with gzip.open(SYNCTEX, "rt", encoding="utf-8", errors="replace") as f:
        for raw in f:
            line = raw.rstrip("\n")
            if line.startswith("Input:"):
                tag, name = line[len("Input:") :].split(":", 1)
                inputs[int(tag)] = name
            elif line.startswith("{"):
                page = int(line[1:])
                pages[page] = []
            elif line.startswith("}"):
                page = None
            elif line.startswith("(") and page is not None:
                m = re.match(r"\(\d+,\d+(?:,-?\d+)?:(-?\d+),(-?\d+):(-?\d+),(-?\d+),(-?\d+)", line)
                if m:
                    h, v, w, ht, dp = (int(g) * UNIT for g in m.groups())
                    if w > 5 and ht + dp > 2:
                        pages[page].append((h, v, w, ht, dp))
    return inputs, pages


def run(args):
    out = subprocess.run(["synctex", *args], capture_output=True, text=True, check=False)
    records, current = [], None
    for line in out.stdout.splitlines():
        if line.startswith("Output:"):
            current = {}
            records.append(current)
        elif current is not None and ":" in line:
            key, value = line.split(":", 1)
            current[key] = value
    return records


def forward(name, line):
    results = []
    for r in run(["view", "-i", f"{line}:0:{name}", "-o", PDF]):
        results.append(
            {
                "page": int(r["Page"]),
                "x": float(r["x"]),
                "y": float(r["y"]),
                "h": float(r["h"]),
                "v": float(r["v"]),
                "width": float(r["W"]),
                "height": float(r["H"]),
            }
        )
    return results


def inverse(page, x, y):
    results = []
    for r in run(["edit", "-o", f"{page}:{x}:{y}:{PDF}"]):
        results.append(
            {"file": normalise(r["Input"]), "line": int(r["Line"]), "column": int(r["Column"])}
        )
    return results


def body_lines(path):
    """1-based line numbers worth querying: the document body of the main file, all of an
    included file."""
    with open(path, encoding="utf-8") as f:
        lines = f.read().split("\n")
    start = 1
    for i, text in enumerate(lines, 1):
        if text.startswith("\\begin{document}"):
            start = i + 1
            break
    end = len(lines)
    while end > start and not lines[end - 1].strip():
        end -= 1
    return list(range(start, end + 1))


def main():
    case, out = sys.argv[1], sys.argv[2]
    forward_count = int(sys.argv[3]) if len(sys.argv) > 3 else 30
    inverse_count = int(sys.argv[4]) if len(sys.argv) > 4 else 40
    rng = random.Random(f"synctex-fixture-{case}")
    inputs, pages = read_synctex()

    # Source files of the project, each under its TeX name (with the `./`), main first.
    names = {}
    for tag in sorted(inputs):
        name = inputs[tag]
        norm = normalise(name)
        if norm.startswith(PROJECT + "/") and norm.endswith(".tex") and norm not in names:
            names[norm] = name
    candidates = [(norm, name, line) for norm, name in names.items() for line in body_lines(norm)]
    if forward_count >= len(candidates):
        chosen = candidates
    else:
        step = len(candidates) / forward_count
        chosen = [candidates[int(i * step + step / 2)] for i in range(forward_count)]
    forward_queries = [
        {"file": norm, "line": line, "results": forward(name, line)} for norm, name, line in chosen
    ]

    # Points: mostly inside lines of text, some anywhere on the page (margins included).
    page_numbers = sorted(pages)
    inverse_queries = []
    for i in range(inverse_count):
        page = page_numbers[i % len(page_numbers)]
        boxes = pages[page]
        if boxes and i % 4 != 3:
            h, v, w, ht, dp = rng.choice(boxes)
            x = h + w * rng.uniform(0.02, 0.98)
            y = v - ht + (ht + dp) * rng.uniform(0.1, 0.9)
        else:
            x = rng.uniform(0, PAGE_WIDTH)
            y = rng.uniform(0, PAGE_HEIGHT)
        x, y = round(x, 2), round(y, 2)
        inverse_queries.append({"page": page, "x": x, "y": y, "results": inverse(page, x, y)})

    with open(out, "w", encoding="utf-8") as f:
        json.dump(
            {"case": case, "pages": len(page_numbers), "forward": forward_queries, "inverse": inverse_queries},
            f,
            indent=1,
        )
        f.write("\n")


if __name__ == "__main__":
    main()
