# syntax=docker/dockerfile:1
FROM rust:1.97-alpine3.22@sha256:df4efa4e0cdfb5245fa06e3f431387b2bcc96782ce5681b7fb6b0297d745bc29 AS build
ARG CDENV_AGENT_BUILD_ID
# The pinned image already contains gcc, binutils and musl-dev.
WORKDIR /source
COPY . .
# Fixed source path and non-PIE static linkage make the two agent artifacts stable.
# An explicit target keeps crt-static flags off host proc-macro/build-script crates.
RUN target="$(rustc -vV | awk '/^host:/ {print $2}')" && \
    CDENV_AGENT_BUILD_ID="$CDENV_AGENT_BUILD_ID" RUSTFLAGS="-C relocation-model=static -C target-feature=+crt-static" \
    cargo build --release --locked --package cdenv-agent --target "$target" && \
    cp "target/$target/release/cdenv-agent" /cdenv-agent

FROM scratch
COPY --from=build /cdenv-agent /cdenv-agent
ENTRYPOINT ["/cdenv-agent"]
