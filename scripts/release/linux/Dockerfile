# Release build environment for the Linux packages.
#
# Built on Ubuntu 22.04 so the binary and the libraries the AppImage carries need
# glibc 2.35 at most. The packages then run on Ubuntu 22.04 and later (and other
# distributions of the same age). Used by build.sh; not needed for development.
FROM ubuntu:22.04

ARG UID=1000
ARG GID=1000
ARG RUST_VERSION=1.100.0
ARG NODE_VERSION=22.23.5

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential pkg-config curl wget file ca-certificates xz-utils rsync \
        libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev libudev-dev \
    && rm -rf /var/lib/apt/lists/*

RUN curl -fsSL "https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-linux-x64.tar.xz" \
        | tar -xJ -C /usr/local --strip-components=1

RUN groupadd -g "${GID}" builder && useradd -m -u "${UID}" -g "${GID}" builder
USER builder
ENV PATH=/home/builder/.cargo/bin:$PATH
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain "${RUST_VERSION}"

# Cache folders. Named volumes mounted here keep the builder's ownership.
RUN mkdir -p /home/builder/.cargo/registry /home/builder/.cache /home/builder/work
WORKDIR /home/builder/work
