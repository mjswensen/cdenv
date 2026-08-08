# syntax=docker/dockerfile:1
FROM debian:13-slim AS base
ARG SPIKE_BUILD_ARG
COPY fixture-content.txt /opt/cdenv-spike/fixture-content.txt
COPY . /tmp/context-check
RUN test "$SPIKE_BUILD_ARG" = "bounded-default" \
    && test "$(cat /opt/cdenv-spike/fixture-content.txt)" = "context-ok" \
    && test ! -e /tmp/context-check/ignored-secret.txt

FROM base AS development
CMD ["/bin/sh", "-c", "while sleep 3600; do :; done"]
