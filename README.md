# Open Prism

An open-source LaTeX editor with AI assistance that works with **any LLM provider**. It is a
self-hosted alternative to OpenAI Prism, which shuts down on January 1, 2027.

> **Status: pre-alpha.** You can edit and compile LaTeX projects and work with an AI
> assistant from any provider. See the [roadmap](#roadmap).

## Quickstart

Open Prism runs on your own computer: the API and the web app listen on `127.0.0.1` only,
and your projects stay in a folder on your disk. Tested on Linux; macOS and Windows are not
tested yet.

**You need:** [Git](https://git-scm.com/),
[rustup](https://rustup.rs/) (the right Rust version is installed on the first build),
[Node.js](https://nodejs.org/) 22 or newer, pnpm (run `corepack enable` once; Corepack ships
with Node.js), [just](https://just.systems), and [Docker](https://docs.docker.com/get-docker/)
for TeX. Your user must be able to run `docker` without `sudo`.

```sh
git clone https://github.com/nttung245/open-prism.git
cd open-prism
just tex-pull   # TeX Live as a Docker image: a 2.6 GB download, about 9 GB on disk; once
just build      # release build of the API and the web app; several minutes the first time
just start      # runs both and prints a sign-in link
```

Open the link it prints (`http://127.0.0.1:3737/login?token=…`). To use other ports:
`OPENPRISM_PORT=8799 WEB_PORT=3799 just start`. After a build, `just start` is the same as
`./target/release/open-prism-api serve --web web/.next/standalone`; see
[Configuration](#configuration) for its options.

Then give the assistant a model:

1. Click **Settings** on the project list. Under **Add a provider**, pick the type, paste
   your API key and click **Add provider**. See [AI providers](#ai-providers) for where to
   get a key, or to use a model on your own machine.
2. Click **Test connection**: it lists the provider's models.
3. Under **Models**, choose a provider and a model for **Agent** (the chat assistant) and,
   if you like, a quicker one for **Fast** (quick actions, suggestions as you type, Image to
   LaTeX; "Same as agent" otherwise). Click **Save models**.
4. Create a project, compile it with `Ctrl+Enter`, and open the assistant with the robot
   button.

Something not working? See [Troubleshooting](#troubleshooting).

## What works today

- Projects: create, rename, delete; import a ZIP (Prism, Overleaf, arXiv) and download one.
- Editor: file tree, tabs, LaTeX highlighting, autosave. A file changed by another program
  reloads by itself; if you had unsaved edits too, you choose which version to keep.
- Compilation with latexmk (pdfLaTeX, XeLaTeX, LuaLaTeX, chosen automatically or per
  project) or Tectonic, on demand (`Ctrl+Enter`) or after every save. Errors and warnings
  appear in the editor and in a Problems list. The automatic choice reads
  `% !TEX program = …`. Otherwise it picks XeLaTeX when a `.tex` file of the project needs
  Unicode fonts (`fontspec`, …) or has Vietnamese text without the T5 encoding
  (`\usepackage[T5]{fontenc}` or `[vietnamese]{babel}`), and pdfLaTeX for the rest.
- PDF preview that keeps your place when it reloads. Click a line of the PDF to jump to its
  source; `Ctrl+Alt+→` in the editor highlights the line in the PDF.
- Capture and ask: drag over part of the PDF (a formula, a table, a figure) and it goes to
  the assistant as an image, with its page and the source line it comes from, ready for
  your question. `Esc` while dragging cancels; a message takes up to four images. The
  conversation's model has to read images (DeepSeek v4.1 Flash through OpenCode Go does);
  one that cannot shows the provider's error.
- Images in the chat: besides PDF captures, paste an image into the message box (when the
  clipboard holds no text), drop image files on the assistant, or pick them with the
  paperclip button. PNG, JPEG and WebP are accepted; images over 2,000 pixels on their
  longest side are scaled down before sending (your files are not changed), and one still
  over 5 MB is refused. The four-image limit of a message counts PDF captures too.
- AI assistant (the robot button): it reads the project with tools (files, search, outline,
  compile log) and **proposes** edits, shown as word-level diffs. You accept or reject each
  change; accepted changes go into the editor, so `Ctrl+Z` undoes them. Nothing is written
  before you decide. "Fix with AI" in the Problems list loops compile → fix → compile.
- Quick actions on a selection (the AI menu): rewrite, shorten, fix grammar, translate,
  explain, or your own instruction.
- Image to LaTeX: paste an image into a `.tex` file (when the clipboard holds no text), drop
  an image file on the editor, or choose "Image to LaTeX…" in the AI menu. The fast model
  transcribes the formula, table, or printed or handwritten text into LaTeX that fits the
  spot: an `equation` or `align` environment (just the math when you are already inside
  one), a `tabular`, or paragraphs in the language of the image. A panel shows the LaTeX as
  it is written; the file does not change until you click Insert, which puts it where you
  pasted or dropped, even if you kept typing meanwhile, as one change that `Ctrl+Z` undoes.
  Packages the result needs that the main file does not load are named, never added, and
  citations come out as `[?]` for you to fill in. The image is sent for that one request and
  is not stored.
- Suggestions as you type: pause in a `.tex` file and the fast model proposes how to go on
  (the rest of the sentence, the next `\item`, a missing `\end{…}`), shown in grey at the
  cursor as the model writes it. `Tab` accepts it, `Ctrl+→` (`Alt+→` on macOS) takes one
  word, `Esc` dismisses it, `Alt+\` asks right away; keep typing and it follows along. Only
  the text around the cursor is sent, and a suggestion never contains a citation: references
  are yours to add. The wand button next to "AI" turns suggestions off. Set the fast role's
  reasoning to "off" in Settings for the quickest suggestions; how soon they appear depends
  mostly on how fast the provider starts answering.
- Version history (the clock button). A version is saved when the project is created or
  imported, before and after you accept AI edits, after a successful compile (at most every
  ten minutes) and around every restore; "Save version…" saves one with a label. Each
  version shows the files it changed with line diffs, or how it differs from the current
  files. Restore one file or the whole project: the current files are saved first, so a
  restore can be undone too.
- Any provider: OpenAI, Anthropic, Gemini, OpenRouter, DeepSeek, Mistral, Groq, xAI, OpenCode
  Go, Ollama or any OpenAI-compatible server (vLLM, LM Studio, llama.cpp). See
  [AI providers](#ai-providers).

### Version history on disk

History lives outside the project, in `<data dir>/history/<project id>.git`, a plain git
repository, so exported ZIPs never contain it. Every file is recorded except symbolic links,
anything named `.git` (your own repository or submodules) and files over 100 MB. To look at it
without Open Prism:

```sh
git --git-dir ~/.local/share/open-prism/history/<project id>.git log --stat
```

### Images sent to the assistant

They are kept in `<data dir>/attachments/<project id>/<conversation id>/`, also outside the
project. Deleting the conversation or the project deletes them. They are sent to the
conversation's AI provider, like the text of the messages.

## AI providers

Add providers in **Settings**. Each one needs a type and, for cloud providers, an API key.
The base URL is filled in; change it only to go through a proxy. **Test connection** lists
the provider's models; you can also type a model name. Model names change often, so this
table does not list them.

| Type in Settings | API key | Notes |
|---|---|---|
| OpenAI | [platform.openai.com/api-keys](https://platform.openai.com/api-keys) | Called through the Responses API, so reasoning models can use tools at any reasoning level |
| Anthropic | [platform.claude.com/settings/keys](https://platform.claude.com/settings/keys) | |
| Google Gemini | [aistudio.google.com/apikey](https://aistudio.google.com/apikey) | Reasoning "off" turns thinking off; models that must think are asked again without it |
| OpenRouter | [openrouter.ai/settings/keys](https://openrouter.ai/settings/keys) | Model names look like `vendor/model` |
| DeepSeek | [platform.deepseek.com/api_keys](https://platform.deepseek.com/api_keys) | |
| Mistral | [admin.mistral.ai/organization/api-keys](https://admin.mistral.ai/organization/api-keys) | |
| Groq | [console.groq.com/keys](https://console.groq.com/keys) | |
| xAI | [console.x.ai](https://console.x.ai/team/default/api-keys) | |
| OpenCode Go | [opencode.ai/console](https://opencode.ai/console), with a Go subscription | A gateway to open models. Open Prism is developed with `deepseek-v4.1-flash` there (November 2026), which also reads images |
| Ollama | none | Models on your machine; see [Local models](#local-models) |
| OpenAI-compatible (vLLM, LM Studio, …) | only if the server asks for one | Base URL required, e.g. `http://127.0.0.1:8000/v1` |

Links checked on 2026-11-20.

### Roles and reasoning

- **Agent** answers the chat and runs "Fix with AI". It reads the project with tools; a model
  that cannot call tools is detected and edits through SEARCH/REPLACE blocks instead.
- **Fast** runs quick actions on a selection, suggestions as you type and Image to LaTeX.
  Left on "Same as agent", these use the agent's model.
- **Reasoning** (default, off, low, medium, high, max) is set per role, and per conversation
  in the assistant. "Default" sends no level. Set Fast to "off" for quick suggestions. When
  a model refuses the chosen level, the request is sent once more: with the nearest level
  the provider lists as supported (OpenAI's `gpt-5.5` tops out at "xhigh", for example), or
  with no level. That costs one extra round trip per request, so pick a level the model has.
- **Images** (PDF captures, images in the chat, Image to LaTeX) need a model that reads
  images. One that cannot shows the provider's error.

### What is sent, and where keys are kept

The provider receives what each feature needs: your messages with their images, the files
and compile log the assistant reads with its tools, the selected text for quick actions, and
the text around the cursor for suggestions. Keys are stored in the database in the data dir
(`open-prism.db`, see [Troubleshooting](#where-is-my-data)) and are never sent back to the
browser.

### Local models

With Ollama in Docker:

```sh
docker run -d --name ollama -p 127.0.0.1:11434:11434 -e OLLAMA_CONTEXT_LENGTH=16384 \
  -v ollama:/root/.ollama ollama/ollama
docker exec ollama ollama pull qwen3:4b
```

Then add an *Ollama* provider in Settings (base URL `http://127.0.0.1:11434`). Set
`OLLAMA_CONTEXT_LENGTH`: Ollama's default context of 4096 tokens silently cuts off the
project context and the tool descriptions.

Other local servers use the *OpenAI-compatible* type, with the base URL ending in `/v1`:

| Server | Base URL by default | Tool calling |
|---|---|---|
| [vLLM](https://docs.vllm.ai/) | `http://127.0.0.1:8000/v1` | Start it with `--enable-auto-tool-choice --tool-call-parser <parser for your model>` |
| [LM Studio](https://lmstudio.ai/docs/developer/openai-compat) | `http://127.0.0.1:1234/v1` | Start the server in the Developer tab or with `lms server start` |
| [llama.cpp](https://github.com/ggml-org/llama.cpp/tree/master/tools/server) `llama-server` | `http://127.0.0.1:9931/v1` (older builds: port 8080) | Needs `--jinja`, on by default |

Without tool calling, the assistant still works through SEARCH/REPLACE blocks.

## Troubleshooting

**"port … on 127.0.0.1 is already in use".** Another program, often an Open Prism still
running, holds the port. Stop it, or pick other ports: `--port` for the API (8787) and
`--web-port` for the web app (3737); with `just start`, set `OPENPRISM_PORT` and `WEB_PORT`.
If `open-prism-api` was killed outright (`kill -9`), its web app keeps running: stop the
leftover `node server.js`.

**"the web app needs Node.js 22 or newer".** Install a newer Node.js, or point to one with
`--node /path/to/node` (or `OPENPRISM_NODE`).

**"No TeX engine found" on the project list.** The TeX engine is looked for when Open Prism
starts, so fix the cause and start it again:
- Docker is not running: start it.
- The image is missing: `just tex-pull`.
- `docker` needs `sudo`: add your user to the `docker` group
  (`sudo usermod -aG docker $USER`), then log out and back in.
- The log line "TeX Live image unavailable" gives Docker's reason.

A native TeX Live (`latexmk`) or [Tectonic](https://tectonic-typesetting.github.io/) works too.

**Lost the sign-in link.** Run `./target/release/open-prism-api print-token` (add the same
`--data-dir` if you use one) and paste the token on the `/login` page, or open
`http://127.0.0.1:3737/login?token=<token>`.

**Letters missing in the PDF (Vietnamese and other accented text).** With the engine on
Auto, Open Prism picks XeLaTeX for Vietnamese text unless the T5 encoding is set up. If a
document still loses letters, choose XeLaTeX or LuaLaTeX in the engine menu, or load the
right `fontenc` and `babel` options for pdfLaTeX.

**The AI says "rate limited".** The provider refused the request for quota or rate reasons;
its own message follows. A monthly spending cap, for example, does not lift by waiting.

### Where is my data

Everything is in one folder: `~/.local/share/open-prism` on Linux, the platform's local data
folder elsewhere, or the folder given with `--data-dir` (`OPENPRISM_DATA_DIR`). It holds
`projects/` (one plain folder per project), `history/`, `attachments/`, the database
`open-prism.db` (settings, provider keys, conversations), `access-token`, and compile output
and caches (`build/`, `runs/`, `cache/`, `tmp/`) that a backup can skip. To back up, stop
Open Prism and copy the folder.

## Configuration

| Variable | Used by | Default |
|---|---|---|
| `OPENPRISM_DATA_DIR` | API | platform data dir, e.g. `~/.local/share/open-prism` |
| `OPENPRISM_PORT` | API | `8787` |
| `OPENPRISM_ACCESS_TOKEN` | API | read from / created in `<data dir>/access-token` |
| `OPENPRISM_TEX_IMAGE` | API, `just tex-pull` | `texlive/texlive:latest` (`none` disables Docker) |
| `OPENPRISM_API_URL` | web (set by `serve --web`) | `http://127.0.0.1:8787` |
| `OPENPRISM_WEB_PORT` | `serve --web` | `3737` |
| `OPENPRISM_NODE` | `serve --web` | `node` from `PATH` |
| `WEB_PORT` | `just dev` / `just start` | `3737` |

Most have a flag too: `--data-dir`, `--port` and `--tex-image`, and after `serve`,
`--web-port` and `--node` (`open-prism-api --help` lists them).

`serve --web` checks for Node.js and the build and that both ports are free. It then starts
the web app and prints the sign-in link once the web app is ready. The web app's output shows
up in the log, marked `web`. If the web app crashes it is restarted; after three crashes within
a minute everything stops with an error. Ctrl+C stops both. Without `--web`, only the API
runs.

**Signing in.** On first start the API creates a random access token in
`<data dir>/access-token` (mode 0600). The sign-in link carries it; the `/login` page also
takes it pasted. Scripts can send it as `Authorization: Bearer <token>`.

## Goals

- Edit LaTeX projects in the browser with a live PDF preview and SyncTeX (click the PDF to
  jump to the source).
- An AI assistant that proposes changes as diffs you accept or reject. It never edits your
  files on its own.
- Bring your own model: OpenAI, Anthropic, Gemini, DeepSeek, Mistral, OpenRouter, Ollama,
  vLLM, LM Studio or any OpenAI-compatible endpoint.
- Your projects stay plain folders of `.tex`/`.bib` files. Import and export ZIP archives,
  including Prism and Overleaf exports.
- Citations are yours: the AI helps with wording and LaTeX syntax, but never adds `\cite`
  commands or looks up papers for you.

## Architecture

```
browser ──► Next.js server (Node 22, :3737) ──/api/*──► open-prism-api (Rust, 127.0.0.1:8787)
            pages, editor UI, API proxy                  projects, compilation (TeX Live in Docker,
                                                         or native latexmk / Tectonic),
                                                         SyncTeX, history, AI (any provider)
```

| Path | What |
|---|---|
| `crates/core` | Projects, compilation, log parsing, SyncTeX, outline, history (no HTTP) |
| `crates/ai` | Providers, model roles, agent loop, tools, patch protocol |
| `crates/literature` | arXiv / Crossref / OpenAlex search, DOI → BibTeX; not used by the app at present |
| `crates/server` | `open-prism-api`: the Axum HTTP API |
| `web` | Next.js app; `web/lib/api/types` is generated from Rust with `ts-rs` |

The API only listens on `127.0.0.1`. The Next.js server is the single entry point and forwards
`/api/*` through a streaming route handler (`web/app/api/[...path]/route.ts`).

## Development

Requirements: Rust 1.98 (installed automatically from `rust-toolchain.toml`), Node.js 22+,
pnpm 12 and [just](https://just.systems). For TeX, `just tex-pull` downloads the official
[TeX Live image](https://hub.docker.com/r/texlive/texlive) (every package), and compilation
runs in a throwaway container without network access. A native TeX Live (`latexmk`) or
[Tectonic](https://tectonic-typesetting.github.io/) installation works too.

```sh
just dev                # API + web app in development mode on 127.0.0.1, prints a sign-in link
just test               # Rust and TypeScript tests, type check
just test docker        # also compile real documents in the TeX Live container
just test-live          # call the real OpenAI and Gemini APIs (keys from .env; a few cents)
just e2e                # end-to-end test in Chromium on the built app (Playwright)
cargo test -p openprism-ai --test ollama -- --ignored --nocapture   # against a local Ollama
just lint               # what CI runs
just gen-types          # regenerate web/lib/api/types after changing API types
```

CI tests the AI layer against fake providers (wiremock), so it needs no keys. `just test-live`
reads `OPENAI_API_KEY` and `GOOGLE_AI_API_KEY` from a `.env` file at the repository root (git
ignores it) or from the environment, and skips a provider without a key.

`just e2e` builds the web app and a debug API, then runs `web/e2e` in Chromium: sign in, add a
provider, import a ZIP, edit, compile, and accept an edit proposed by a fake OpenAI-compatible
model that the test runs itself. It uses ports 3798, 8798 and 8797 (`E2E_WEB_PORT`,
`E2E_API_PORT`, `E2E_MODEL_PORT`) and a fresh data dir in `web/test-results/`, where the
server log, traces and screenshots of a failed run stay. Run `pnpm exec playwright install
chromium` in `web/` once first.

## Roadmap

- [x] **M0 Bootstrap**: workspace, API skeleton, access-token sign-in, API proxy, CI
- [x] **M1 Editor and compilation**: projects, ZIP import/export, CodeMirror editor,
      latexmk/Tectonic compilation, error list, PDF preview, SyncTeX
- [x] **M2 AI**: provider settings, chat with project context, reviewed edits,
      "fix compile errors", fallback for models without tool calling
- [x] **M3 Writing help**: version history, suggestions as you type, capture and ask
      about the PDF, images in the chat, image and handwriting to LaTeX
- [ ] **M4 Release**: one-command run and user docs (done), prebuilt binaries, Docker image

## License

[MIT](LICENSE)
