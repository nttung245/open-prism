# Open Prism with TeX Live: the Linux release package (scripts/package-release.sh) on the official
# TeX Live image. The release workflow builds it from the package it has just tested:
#
#   docker build -f Dockerfile --build-arg PACKAGE=open-prism-<version>-linux-<x64|arm64>.tar.gz \
#     --build-arg VERSION=<version> <folder holding only that package>
#
# `just image` does the same on this machine. Run it with compose.yaml (see the README).
#
# The TeX Live image is pinned by digest (an index for amd64 and arm64); change it here, in a
# reviewed commit, to move to a newer TeX Live.
ARG BASE=texlive/texlive:latest@sha256:c21636aa13e86716d15dab4e352356cb0d34f9365d6091db066e5df71041d67b

# Unpacked in a stage of its own, so the final image holds the folder once.
FROM ${BASE} AS unpack
ARG PACKAGE
ADD ${PACKAGE} /tmp/package/
RUN mv /tmp/package/open-prism-* /opt/open-prism

FROM ${BASE}
ARG VERSION
LABEL org.opencontainers.image.title="Open Prism" \
      org.opencontainers.image.description="LaTeX editor with AI assistance that works with any LLM provider" \
      org.opencontainers.image.source="https://github.com/nttung245/open-prism" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.version="${VERSION}"

# The app runs as the image's own user `texlive` (uid 1000), not root; data lives in /data.
RUN install -d -o texlive -g texlive -m 700 /data
COPY --from=unpack /opt/open-prism /opt/open-prism

# Compile with the TeX Live in this image (no Docker inside), never try to open a browser, and
# let Docker reach the web app: the API itself stays on 127.0.0.1 inside the container.
ENV PATH=/opt/open-prism:$PATH \
    OPENPRISM_DATA_DIR=/data \
    OPENPRISM_TEX_IMAGE=none \
    OPENPRISM_NO_BROWSER=1 \
    OPENPRISM_WEB_HOST=0.0.0.0
USER texlive
WORKDIR /home/texlive
EXPOSE 3737
HEALTHCHECK --interval=30s --timeout=5s --start-period=60s --retries=3 \
  CMD ["/opt/open-prism/node/bin/node", "-e", "fetch(`http://127.0.0.1:${process.env.OPENPRISM_WEB_PORT || 3737}/api/health`).then(r => process.exit(r.ok ? 0 : 1), () => process.exit(1))"]
ENTRYPOINT ["/opt/open-prism/open-prism"]
