Open Prism {version}: a LaTeX editor with AI assistance that works with any LLM provider.
https://github.com/nttung245/open-prism

What you need
  Linux with glibc 2.35 or newer (Ubuntu 22.04+, Debian 12+, Fedora 36+), x64 or arm64.
  Docker, to compile LaTeX: download TeX Live once with
      docker pull texlive/texlive:latest
  (about 2.6 GB). A TeX Live or Tectonic installed on the machine works too.
  Nothing else: this folder holds Open Prism and the Node.js it runs on.

Run it
      ./open-prism
  Your browser opens on the sign-in page once Open Prism is ready (the link is also printed).
  Stop it with Ctrl+C, or by closing the terminal window.

  Other ports or another data folder:
      ./open-prism --port 8799 --web-port 3799 --data-dir ~/prism-data
  Without opening the browser (over SSH, on a machine without a screen):
      OPENPRISM_NO_BROWSER=1 ./open-prism
  The sign-in token, if you lost the link:
      ./open-prism print-token

In the application menu
      ./open-prism install-desktop      adds "Open Prism" to the menu
      ./open-prism uninstall-desktop    removes it
  It starts in a terminal window: close the window to stop Open Prism. Clicking it again while
  Open Prism runs only opens the browser. If you move this folder, run install-desktop again.

Your data
  Projects, history and settings live in ~/.local/share/open-prism (or the --data-dir folder),
  not in this folder: you can replace this folder with a newer version safely.

Check the download
  In the folder with the .tar.gz files and SHA256SUMS:
      sha256sum -c SHA256SUMS --ignore-missing
  With the GitHub CLI, check it was built from the repository's source:
      gh attestation verify open-prism-{version}-linux-{arch}.tar.gz --repo nttung245/open-prism

More: AI providers, troubleshooting and the full documentation are in the README on GitHub.
License: Apache License 2.0 (LICENSE, NOTICE). Node.js is under its own license (node/LICENSE).
