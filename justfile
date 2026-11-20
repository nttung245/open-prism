# Open Prism task runner — https://just.systems
set shell := ["bash", "-euo", "pipefail", "-c"]

api_port := env("OPENPRISM_PORT", "8787")
web_port := env("WEB_PORT", "3737")
api_url := "http://127.0.0.1:" + api_port
tex_image := env("OPENPRISM_TEX_IMAGE", "texlive/texlive:latest")

# List recipes
default:
    @just --list

# Install web dependencies
setup:
    pnpm -C web install --frozen-lockfile

# Download TeX Live as a Docker image (2.6 GB download, ~9 GB on disk; nothing installed on the host)
tex-pull:
    docker pull {{tex_image}}

# Run the API and the web app in development mode (both on 127.0.0.1)
dev: setup
    #!/usr/bin/env bash
    set -euo pipefail
    trap 'kill 0' EXIT INT TERM
    cargo build -p openprism-server
    echo "Sign in at: http://127.0.0.1:{{web_port}}/login?token=$(./target/debug/open-prism-api print-token)"
    ./target/debug/open-prism-api --port {{api_port}} &
    OPENPRISM_API_URL={{api_url}} pnpm -C web exec next dev -H 127.0.0.1 -p {{web_port}} &
    wait

# Production build: release API binary + standalone Next.js server
build: setup build-web
    cargo build --release -p openprism-server

# Standalone build of the web app, with the files `next build` leaves out of it
build-web: setup
    pnpm -C web build
    cp -r web/public web/.next/standalone/
    cp -r web/.next/static web/.next/standalone/.next/

# Run the production build locally: the API starts the web app and prints a sign-in link (both on 127.0.0.1)
start:
    ./target/release/open-prism-api serve --web web/.next/standalone --port {{api_port}} --web-port {{web_port}}

# Release package for this machine (Linux x64 or arm64) in target/dist, as the release workflow makes it, then run it once
package: build
    #!/usr/bin/env bash
    set -euo pipefail
    case "$(uname -m)" in x86_64) arch=x64 ;; aarch64) arch=arm64 ;; *) echo "no release package for $(uname -m)" >&2; exit 1 ;; esac
    id=$(cargo pkgid -p openprism-server)
    version=${id##*[@#]}
    scripts/package-release.sh "$version" "$arch"
    scripts/smoke-release.sh "target/dist/open-prism-$version-linux-$arch.tar.gz"

# Print the access token
token:
    @cargo run -q -p openprism-server -- print-token

# Run all tests (add `docker` to also compile real documents in the TeX Live container)
test *mode:
    cargo test --workspace {{ if mode == "docker" { "-- --include-ignored" } else { "" } }}
    pnpm -C web typecheck
    pnpm -C web test

# Call the real OpenAI and Gemini APIs with the keys in .env or the environment (a few cents per run)
test-live:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -f .env ]; then set -a; source .env; set +a; fi
    OPENPRISM_LIVE=1 cargo test -p openprism-ai --test live -- --ignored --nocapture --test-threads 1

# End-to-end tests in Chromium against the built app (debug API, standalone web app)
e2e *args: build-web
    cargo build -p openprism-server
    pnpm -C web e2e {{args}}

# Lint everything (what CI runs)
lint:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    pnpm -C web exec biome ci .
    pnpm -C web typecheck

# Format everything
fmt:
    cargo fmt --all
    pnpm -C web exec biome check --write .

# Regenerate TypeScript types from Rust (web/lib/api/types)
gen-types:
    cargo test --workspace export_bindings
