# Open Prism

An open-source LaTeX editor with AI assistance that works with **any LLM provider**. It is a
self-hosted alternative to OpenAI Prism, which shuts down on January 1, 2027.

> **Status: pre-alpha.** You can edit and compile LaTeX projects and work with an AI
> assistant from any provider. See the [roadmap](#roadmap).

## Quickstart

Open Prism runs on your own computer: the API and the web app listen on `127.0.0.1` only,
and your projects stay in a folder on your disk. On Linux, download a release; on macOS and
Windows, run the Docker image; or build it from source anywhere.

### Download a release (Linux)

**You need:** Linux x64 or arm64 with glibc 2.35 or newer (Ubuntu 22.04, Debian 12, Fedora 36
or newer) and [Docker](https://docs.docker.com/get-docker/) for TeX. Your user must be able to
run `docker` without `sudo`. The package brings everything else, Node.js included.

Download `open-prism-<version>-linux-x64.tar.gz` (or `-arm64`) from the
[Releases page](https://github.com/nttung245/open-prism/releases), then:

```sh
docker pull texlive/texlive:latest   # TeX Live: a 2.6 GB download, about 9 GB on disk; once
tar -xzf open-prism-<version>-linux-x64.tar.gz
cd open-prism-<version>-linux-x64
./open-prism                         # starts Open Prism and opens the sign-in page
```

Your browser opens once Open Prism is ready; the sign-in link is printed too. Stop it with
Ctrl+C or by closing the terminal window. `./open-prism install-desktop` adds **Open Prism**
to the application menu (it runs in a terminal window; close the window to stop it), and
`./open-prism uninstall-desktop` removes it. If you move the folder, run `install-desktop`
again. `README.txt` in the folder lists the other commands, such as other ports and
`OPENPRISM_NO_BROWSER=1` for a machine without a screen.

To check the download, run this in the folder with the `.tar.gz` and `SHA256SUMS` (from the
same release) and, with the [GitHub CLI](https://cli.github.com/), check that GitHub Actions
built it from this repository:

```sh
sha256sum -c SHA256SUMS --ignore-missing
gh attestation verify open-prism-<version>-linux-x64.tar.gz --repo nttung245/open-prism
```

Projects and settings live in `~/.local/share/open-prism`, not in the release folder, so a
newer release can replace the folder.

### Run with Docker (macOS, Windows, Linux)

**You need:** [Docker](https://docs.docker.com/get-docker/) (Docker Desktop on macOS and
Windows). The image holds everything, TeX Live included: about 2.7 GB to download and 9.5 GB on
disk. It runs on amd64 and arm64 (Apple Silicon too). Images are published from version 0.2.0
on.

Save [`compose.yaml`](compose.yaml) in a folder, then in that folder:

```sh
docker compose up -d               # downloads the image the first time, then starts Open Prism
docker compose logs open-prism     # shows the sign-in link
```

Open the link it shows (`http://127.0.0.1:3737/login?token=…`);
`docker compose exec open-prism open-prism print-token` prints the token again. Stop it with
`docker compose down`. Projects and settings stay in the `open-prism-data` volume, so you can
replace or upgrade the container: `docker compose pull && docker compose up -d`.

Without compose:

```sh
docker run -d --name open-prism --init -p 127.0.0.1:3737:3737 -v open-prism-data:/data \
  ghcr.io/nttung245/open-prism:latest
docker logs open-prism             # shows the sign-in link
```

Keep `127.0.0.1:` in front of the port: without it Docker opens Open Prism to your whole
network. For another port, change both sides and tell the app, as in
`-p 127.0.0.1:4000:4000 -e OPENPRISM_WEB_PORT=4000`.

To back up, stop Open Prism, then copy the data out:
`docker compose stop && docker compose cp open-prism:/data ./open-prism-backup` (or
`docker cp open-prism:/data ./open-prism-backup` without compose). A folder of your own instead
of the volume (`-v ./data:/data`) works only if the user with uid 1000 can write to it.

The image is the Linux release package on the official
[TeX Live image](https://hub.docker.com/r/texlive/texlive): Open Prism compiles inside the
container and never needs your Docker socket. `latest` is the newest published release;
`<version>` tags stay as they are.

### Build from source

Tested on Linux; macOS and Windows are not tested yet.

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

### Give the assistant a model

Once signed in, either way:

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

- Projects: create, rename, delete; import a ZIP (Prism, Overleaf, arXiv) or a PDF; download
  one as ZIP or 7z.
- Import a PDF as a LaTeX project: **Import PDF** on the project list takes the file, a name
  (the file name by default) and the pages (empty for all of them, or a list such as
  `1-5, 8`, up to 300 pages and 200 MB). The Agent model reads each page as an image, with
  the PDF's text layer when there is one (scans work too). It writes a preamble from the
  first page so the result looks like the original: paper size, margins, a close font,
  heading style, lists. Pictures are cut out of the page into `figures/`. The pages are put
  together and compiled, and the Agent fixes compile errors (up to two rounds); the result
  is a new project with a first version in its history. Citations stay as printed text, never
  `\cite`: references are yours to add.

  The import runs in the background. A list above your projects shows each job with its
  pages done, live: keep working in other projects meanwhile, **Cancel** it, **Retry** a
  failed one (pages already copied are kept, so fix the key in Settings and retry), then
  **Open project**. It keeps going if you close the page, and after a restart.

  With DeepSeek v4.1 Flash through OpenCode Go, a 3-page paper took 30–60 seconds. Each page
  is one request of about 2–4k input tokens (mostly the image) and 0.2–1.5k output tokens,
  plus one request for the preamble. The Agent's model has to read images.
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
  Download the whole PDF, or only some pages: "Download pages…" takes a list such as
  `3-7, 10` (filled in with the page you are reading) and gives a smaller PDF of those pages.
  Links between the pages you keep still work; bookmarks are dropped.
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
- **Images** (PDF captures, images in the chat, Image to LaTeX, PDF import) need a model that
  reads images. One that cannot shows the provider's error.
- **PDF import** uses the Agent's model, with reasoning off while it copies pages: thinking
  spends the output tokens a page needs.

### What is sent, and where keys are kept

The provider receives what each feature needs: your messages with their images, the files
and compile log the assistant reads with its tools, the selected text for quick actions, and
the text around the cursor for suggestions. A PDF import sends each page as an image with
its text layer, and the compile errors of the result with its preamble. Keys are stored in the database in the data dir
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
`--web-port` for the web app (3737), as in `./open-prism --port 8799 --web-port 3799`; with
`just start`, set `OPENPRISM_PORT` and `WEB_PORT`. (With `--open`, which `./open-prism` uses,
an Open Prism already running on those ports is not an error: the sign-in page just opens
again.)
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

**Lost the sign-in link.** Run `./open-prism print-token` in the release folder,
`docker compose exec open-prism open-prism print-token` with Docker, or
`./target/release/open-prism-api print-token` in a source build (add the same `--data-dir` if
you use one), and paste the token on the `/login` page, or open
`http://127.0.0.1:3737/login?token=<token>`.

**The browser does not open.** `./open-prism` opens the sign-in page with `$BROWSER` if it is
set, otherwise `xdg-open`. If neither works, the log says "cannot open the browser" and Open
Prism keeps running: open the printed link yourself.

**Letters missing in the PDF (Vietnamese and other accented text).** With the engine on
Auto, Open Prism picks XeLaTeX for Vietnamese text unless the T5 encoding is set up. If a
document still loses letters, choose XeLaTeX or LuaLaTeX in the engine menu, or load the
right `fontenc` and `babel` options for pdfLaTeX.

**The AI says "rate limited".** The provider refused the request for quota or rate reasons;
its own message follows. A monthly spending cap, for example, does not lift by waiting.

### Where is my data

Everything is in one folder: `~/.local/share/open-prism` on Linux, the platform's local data
folder elsewhere, `/data` in the Docker image (the `open-prism-data` volume), or the folder
given with `--data-dir` (`OPENPRISM_DATA_DIR`). It holds
`projects/` (one plain folder per project), `history/`, `attachments/`, the database
`open-prism.db` (settings, provider keys, conversations, background jobs), `access-token`,
`jobs/` (working files of background jobs; a backup can skip it when no job is unfinished), and
compile output and caches (`build/`, `runs/`, `cache/`, `tmp/`) that a backup can skip. To back
up, stop Open Prism and copy the folder.

## Configuration

| Variable | Used by | Default |
|---|---|---|
| `OPENPRISM_DATA_DIR` | API | platform data dir, e.g. `~/.local/share/open-prism` |
| `OPENPRISM_PORT` | API | `8787` |
| `OPENPRISM_ACCESS_TOKEN` | API | read from / created in `<data dir>/access-token` |
| `OPENPRISM_TEX_IMAGE` | API, `just tex-pull` | `texlive/texlive:latest` (`none` disables Docker) |
| `OPENPRISM_API_URL` | web (set by `serve --web`) | `http://127.0.0.1:8787` |
| `OPENPRISM_WEB_PORT` | `serve --web` | `3737` |
| `OPENPRISM_WEB_HOST` | `serve --web` | `127.0.0.1` (`0.0.0.0` in the Docker image) |
| `OPENPRISM_NODE` | `serve --web` | `node` from `PATH` |
| `OPENPRISM_OPEN` | `serve --web` | off (`true` is the same as `--open`) |
| `OPENPRISM_NO_BROWSER` | `./open-prism` in a release | unset (`1` leaves out `--open`) |
| `WEB_PORT` | `just dev` / `just start` | `3737` |

Most have a flag too: `--data-dir`, `--port` and `--tex-image`, and after `serve`,
`--web-port`, `--web-host`, `--node` and `--open` (`open-prism-api --help` lists them).

`--web-host` takes `127.0.0.1` (this computer only) or `0.0.0.0` (every address). Use `0.0.0.0`
only in a container whose port is published on the host's `127.0.0.1`, as the Docker image
does. The API always listens on `127.0.0.1`.

`serve --web` checks for Node.js and the build and that both ports are free. It then starts
the web app and prints the sign-in link once the web app is ready. The web app's output shows
up in the log, marked `web`. If the web app crashes it is restarted; after three crashes within
a minute everything stops with an error. Ctrl+C, SIGTERM, or closing the terminal (SIGHUP)
stops both. Without `--web`, only the API runs.

With `--open`, the sign-in link also opens in the browser (`$BROWSER`, otherwise `xdg-open`,
`open` on macOS or `start` on Windows). If an Open Prism is already running on those ports,
`serve --open` opens its sign-in page and exits instead of failing. The `open-prism` script of
a release runs `serve --web` with the bundled web app and Node.js, and adds `--open` unless
`OPENPRISM_NO_BROWSER=1`.

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

Long work runs as **background jobs** (`crates/server/src/jobs`), in steps whose state is kept
in the database, with working files in `jobs/<id>/` outside the projects. One job runs at a
time; a job cut short by stopping Open Prism resumes at the next start without redoing finished
steps. Its AI requests go through a gate that allows three at a time per provider and waits out
rate limits, so chat and suggestions never queue behind a job. `GET /api/jobs` lists jobs and
`GET /api/events` streams their progress to every open page.

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
just package            # release package for this machine in target/dist, then run it once
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

### Releasing

The [Release workflow](.github/workflows/release.yml) builds a package for Linux x64 and one
for arm64 on Ubuntu 22.04: the API, the standalone web app, the official Node.js
(`scripts/package-release.sh`, which pins its version and SHA-256) and the
[`open-prism` script](packaging/open-prism). It then runs each package once
(`scripts/smoke-release.sh`): start, the web app and the API answer, a clean stop. `just
package` does the same for your machine.

From each package it builds the Docker image ([`Dockerfile`](Dockerfile): the package on the
official TeX Live image, pinned by digest) and runs it on that platform
(`scripts/smoke-image.sh`): sign in, compile the article template to a PDF, the health check,
a clean `docker stop`, and the data still there in a new container. `just image` does the same
for your machine.

1. Set the new `version` in `Cargo.toml` (`[workspace.package]`). Check for a newer Node.js 22
   release with security fixes, and if there is one, update `NODE_VERSION` and both checksums
   in `scripts/package-release.sh`. For a newer TeX Live in the image, set the digest of `BASE`
   in `Dockerfile` to the one `docker pull texlive/texlive:latest` prints.
2. Merge to `main`.
3. Run the workflow by hand (Actions → Release → Run workflow): it builds and tests both
   packages and both images without releasing or pushing anything. Pull requests that change
   the packaging or the image run it too.
4. Push the tag: `git tag v<version> && git push origin v<version>`. The workflow refuses a tag
   that differs from `Cargo.toml`. When both packages and both images pass, it pushes the image
   as `ghcr.io/nttung245/open-prism:<version>` (and `<version>-amd64`, `<version>-arm64`), and
   makes a **draft** release with the packages, `SHA256SUMS` and build provenance attestations
   (for the image too: `gh attestation verify oci://ghcr.io/nttung245/open-prism:<version>
   --repo nttung245/open-prism`).
5. Look over the draft on the Releases page, then publish it. Publishing moves the image's
   `latest` tag to this version. After the first image ever pushed, check that
   `docker pull ghcr.io/nttung245/open-prism:<version>` works without signing in; if not, make
   the `open-prism` package public in its settings on GitHub.

## Roadmap

- [x] **M0 Bootstrap**: workspace, API skeleton, access-token sign-in, API proxy, CI
- [x] **M1 Editor and compilation**: projects, ZIP import/export, CodeMirror editor,
      latexmk/Tectonic compilation, error list, PDF preview, SyncTeX
- [x] **M2 AI**: provider settings, chat with project context, reviewed edits,
      "fix compile errors", fallback for models without tool calling
- [x] **M3 Writing help**: version history, suggestions as you type, capture and ask
      about the PDF, images in the chat, image and handwriting to LaTeX
- [x] **M4 Release**: one-command run, user docs, prebuilt Linux packages, Docker image
- [x] **PDF import**: background jobs, and importing a PDF (scanned too) as a LaTeX project,
      the Agent reading each page

## License

[Apache License 2.0](LICENSE). See [NOTICE](NOTICE).
