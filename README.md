# Open Prism

An open-source LaTeX editor with AI assistance that works with **any LLM provider**. It is a
self-hosted alternative to OpenAI Prism, which shuts down on January 1, 2027.

> **Status: pre-alpha.** You can edit and compile LaTeX projects and work with an AI
> assistant from any provider. See the [roadmap](#roadmap).

## What works today

- Projects: create, rename, delete; import a ZIP (Prism, Overleaf, arXiv) and download one.
- Editor: file tree, tabs, LaTeX highlighting, autosave. A file changed by another program
  reloads by itself; if you had unsaved edits too, you choose which version to keep.
- Compilation with latexmk (pdfLaTeX, XeLaTeX, LuaLaTeX, chosen automatically or per
  project) or Tectonic, on demand (`Ctrl+Enter`) or after every save. Errors and warnings
  appear in the editor and in a Problems list.
- PDF preview that keeps your place when it reloads. `Ctrl`/`⌘`-click or double-click the
  PDF to jump to the source; `Ctrl+Alt+→` in the editor highlights the line in the PDF.
- AI assistant (the robot button): it reads the project with tools (files, search, outline,
  compile log) and **proposes** edits, shown as word-level diffs. You accept or reject each
  change; accepted changes go into the editor, so `Ctrl+Z` undoes them. Nothing is written
  before you decide. "Fix with AI" in the Problems list loops compile → fix → compile.
- Quick actions on a selection (the AI menu): rewrite, shorten, fix grammar, translate,
  explain, or your own instruction.
- Version history (the clock button). A version is saved when the project is created or
  imported, before and after you accept AI edits, after a successful compile (at most every
  ten minutes) and around every restore; "Save version…" saves one with a label. Each
  version shows the files it changed with line diffs, or how it differs from the current
  files. Restore one file or the whole project: the current files are saved first, so a
  restore can be undone too.
- Any provider: OpenAI, Anthropic, Gemini, OpenRouter, DeepSeek, Mistral, Groq, xAI, OpenCode
  Go, Ollama or any OpenAI-compatible server (vLLM, LM Studio, llama.cpp). Models that cannot call tools
  are detected and switch to SEARCH/REPLACE edits automatically. Configure them in Settings,
  including the reasoning effort per role (off, low, medium, high, max), which can also be
  changed per conversation. API keys stay on the server and are never sent back to the
  browser.

### Local models with Docker

```sh
docker run -d --name ollama -p 127.0.0.1:11434:11434 -e OLLAMA_CONTEXT_LENGTH=16384 \
  -v ollama:/root/.ollama ollama/ollama
docker exec ollama ollama pull qwen3:4b
```

Then add an *Ollama* provider in Settings (base URL `http://127.0.0.1:11434`). Set
`OLLAMA_CONTEXT_LENGTH`: Ollama's default context of 4096 tokens silently cuts off the
project context and the tool descriptions.

### Version history on disk

History lives outside the project, in `<data dir>/history/<project id>.git`, a plain git
repository, so exported ZIPs never contain it. Every file is recorded except symbolic links,
anything named `.git` (your own repository or submodules) and files over 100 MB. To look at it
without Open Prism:

```sh
git --git-dir ~/.local/share/open-prism/history/<project id>.git log --stat
```

## Goals

- Edit LaTeX projects in the browser with a live PDF preview and SyncTeX (click the PDF to
  jump to the source).
- An AI assistant that proposes changes as diffs you accept or reject. It never edits your
  files on its own.
- Bring your own model: OpenAI, Anthropic, Gemini, DeepSeek, Mistral, OpenRouter, Ollama,
  vLLM, LM Studio or any OpenAI-compatible endpoint.
- Your projects stay plain folders of `.tex`/`.bib` files. Import and export ZIP archives,
  including Prism and Overleaf exports.
- Citations added by the AI are checked against DOI / arXiv records.

## Architecture

```
browser ──► Next.js server (Node 22, :3737) ──/api/*──► open-prism-api (Rust, 127.0.0.1:8787)
            pages, editor UI, API proxy                  projects, compilation (TeX Live in Docker,
                                                         or native latexmk / Tectonic),
                                                         SyncTeX, AI (any provider), literature
```

| Path | What |
|---|---|
| `crates/core` | Projects, compilation, log parsing, SyncTeX, outline, history (no HTTP) |
| `crates/ai` | Providers, model roles, agent loop, tools, patch protocol |
| `crates/literature` | arXiv / Crossref / OpenAlex search, DOI → BibTeX |
| `crates/server` | `open-prism-api`: the Axum HTTP API |
| `web` | Next.js app; `web/lib/api/types` is generated from Rust with `ts-rs` |

The API only listens on `127.0.0.1`. The Next.js server is the single entry point and forwards
`/api/*` through a streaming route handler (`web/app/api/[...path]/route.ts`).

## Development

Requirements: Rust 1.98 (installed automatically from `rust-toolchain.toml`), Node.js 22+,
pnpm 12 and [just](https://just.systems).

To compile documents you need TeX. The simplest option is [Docker](https://docs.docker.com/get-docker/):
`just tex-pull` downloads the official [TeX Live image](https://hub.docker.com/r/texlive/texlive)
(every package; a 2.6 GB download, about 9 GB on disk), and compilation then runs in a
throwaway container without network access. A native TeX Live (`latexmk`) or
[Tectonic](https://tectonic-typesetting.github.io/) installation works too.

```sh
just tex-pull           # once: TeX Live through Docker
just dev                # API + web app on 127.0.0.1, prints a sign-in link
just test               # Rust and TypeScript tests, type check
just test docker        # also compile real documents in the TeX Live container
cargo test -p openprism-ai --test ollama -- --ignored --nocapture   # against a local Ollama
just lint               # what CI runs
just gen-types          # regenerate web/lib/api/types after changing API types
just build && just start  # production build, run locally
```

### Signing in

On first start the API creates a random access token in `<data dir>/access-token`
(mode 0600). Open the sign-in link printed by `just dev`, or run `just token` and paste the
token on the `/login` page. Scripts can send it as `Authorization: Bearer <token>`.

### Configuration

| Variable | Used by | Default |
|---|---|---|
| `OPENPRISM_DATA_DIR` | API | platform data dir, e.g. `~/.local/share/open-prism` |
| `OPENPRISM_PORT` | API | `8787` |
| `OPENPRISM_ACCESS_TOKEN` | API | read from / created in `<data dir>/access-token` |
| `OPENPRISM_TEX_IMAGE` | API, `just tex-pull` | `texlive/texlive:latest` (`none` disables Docker) |
| `OPENPRISM_API_URL` | web | `http://127.0.0.1:8787` |
| `WEB_PORT` | `just dev` / `just start` | `3737` |

## Roadmap

- [x] **M0 Bootstrap**: workspace, API skeleton, access-token sign-in, API proxy, CI
- [x] **M1 Editor and compilation**: projects, ZIP import/export, CodeMirror editor,
      latexmk/Tectonic compilation, error list, PDF preview, SyncTeX
- [x] **M2 AI**: provider settings, chat with project context, reviewed edits,
      "fix compile errors", fallback for models without tool calling
- [ ] **M3 Research**: version history (done), literature search and verified citations,
      image/handwriting to LaTeX, inline completion
- [ ] **M4 Release**: password sign-in, Docker images, prebuilt binaries

## License

[MIT](LICENSE)
