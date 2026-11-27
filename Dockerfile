# syntax=docker/dockerfile:1
#
# The CPU image of `rueca serve` (RFC 0008 §4.3): the release binary on
# distroless (glibc + libgcc), plus `git` (≥ 2.45, §7.6) and the libraries
# it and `git-remote-https` link, copied from a Debian trixie stage.
#
#   docker build -t rueca .
#   docker run --rm -p 8080:8080 -p 9090:9090 \
#     -v "$PWD/rueca-service.toml:/etc/rueca/rueca-service.toml:ro" \
#     -v rueca-cache:/var/cache/rueca --read-only rueca
#
# git here speaks HTTPS (what the indexer fetches, RFC 0008 §7.6): there is
# no shell and no ssh, so `ssh://` remotes, local-path clones (git runs
# `git-upload-pack` through /bin/sh) and shell credential helpers do not
# work.
#
# Base images are pinned by digest (Dependabot `docker` updates the
# digests; the Rust minor stays the MSRV, 1.90); the tags are kept for
# readers.

# --- build: the release binary ---------------------------------------------------------
FROM rust:1.90-trixie@sha256:e227f20ec42af3ea9a3c9c1dd1b2012aa15f12279b5e9d5fb890ca1c2bb5726c AS build
WORKDIR /src
# The default `embeddings` feature builds llama.cpp (CMake, C/C++) and its
# bindings (bindgen: libclang); the runtime only runs when asked.
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake clang libclang-dev \
    && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo build --release -p rueca --locked \
    && install -D -m 0755 target/release/rueca /out/usr/local/bin/rueca

# --- git: the binary, the HTTPS helper and every library they link ---------------------
FROM debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2 AS git
RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates \
    && rm -rf /var/lib/apt/lists/*
# Assert the version §7.6 needs, then stage git, its HTTP(S) remote helper
# (`git-remote-http` and `git-remote-https` are one binary) and the shared
# libraries `ldd` lists for them, at their canonical /usr paths. glibc and
# libgcc are left out: distroless/cc has them, from the same Debian release,
# and mixing two builds of glibc's pieces is what would break.
RUN set -eu; \
    v="$(git --version | awk '{print $3}')"; \
    major="${v%%.*}"; rest="${v#*.}"; minor="${rest%%.*}"; \
    if [ "$major" -lt 2 ] || { [ "$major" -eq 2 ] && [ "$minor" -lt 45 ]; }; then \
        echo "git $v is older than 2.45" >&2; exit 1; \
    fi; \
    bins="/usr/bin/git /usr/lib/git-core/git-remote-http /usr/lib/git-core/git-remote-https"; \
    mkdir -p /stage/usr/share/git-core /stage/var/cache/rueca /stage/etc/rueca; \
    cp -a --parents $bins /stage; \
    ldd $bins | awk '$2 == "=>" && $3 ~ /^\// {print $3}' | sort -u | while read -r lib; do \
        case "$(basename "$lib")" in \
            libc.so.*|libm.so.*|libpthread.so.*|libdl.so.*|librt.so.*|libresolv.so.*|ld-linux*|libgcc_s.so.*|libstdc++.so.*) continue ;; \
        esac; \
        real="$(readlink -f "$lib")"; \
        canon="/usr${real#/usr}"; \
        mkdir -p "/stage$(dirname "$canon")"; \
        cp -L "$lib" "/stage$(dirname "$canon")/$(basename "$lib")"; \
    done; \
    cp -a /usr/share/git-core/templates /stage/usr/share/git-core/

# --- runtime ---------------------------------------------------------------------------
FROM gcr.io/distroless/cc-debian13:nonroot@sha256:29ee70762214fe8190cea04ef1c0a84a8365879e047ce8f911ae5a5ab2355fa2
COPY --from=git /stage/usr /usr
COPY --from=git /stage/etc/rueca /etc/rueca
COPY --from=git --chown=65532:65532 /stage/var/cache/rueca /var/cache/rueca
COPY --from=build /out/usr/local/bin/rueca /usr/local/bin/rueca
USER 65532:65532
ENV RUECA_CONFIG=/etc/rueca/rueca-service.toml
VOLUME ["/var/cache/rueca"]
EXPOSE 8080 9090
ENTRYPOINT ["rueca"]
CMD ["serve", "--config", "/etc/rueca/rueca-service.toml"]
