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
build: setup
    cargo build --release -p openprism-server
    pnpm -C web build
    cp -r web/public web/.next/standalone/
    cp -r web/.next/static web/.next/standalone/.next/

# Run the production build locally: the API starts the web app and prints a sign-in link (both on 127.0.0.1)
start:
    ./target/release/open-prism-api serve --web web/.next/standalone --port {{api_port}} --web-port {{web_port}}

# Print the access token
token:
    @cargo run -q -p openprism-server -- print-token

# Run all tests (add `docker` to also compile real documents in the TeX Live container)
test *mode:
    cargo test --workspace {{ if mode == "docker" { "-- --include-ignored" } else { "" } }}
    pnpm -C web typecheck
    pnpm -C web test

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
