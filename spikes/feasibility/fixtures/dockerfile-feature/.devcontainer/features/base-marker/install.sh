#!/bin/sh
set -eu
mkdir -p /usr/local/share/cdenv-spike
printf '%s\n' "${MESSAGE:-base}" > /usr/local/share/cdenv-spike/base-marker
