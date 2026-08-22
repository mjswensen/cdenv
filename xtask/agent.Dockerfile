# syntax=docker/dockerfile:1
FROM rust:1.97-alpine3.22 AS build
ARG CDENV_AGENT_BUILD_ID
RUN apk add --no-cache build-base
WORKDIR /source
COPY . .
RUN CDENV_AGENT_BUILD_ID="$CDENV_AGENT_BUILD_ID" cargo build --release --locked --package cdenv-agent

FROM scratch
COPY --from=build /source/target/release/cdenv-agent /cdenv-agent
ENTRYPOINT ["/cdenv-agent"]
