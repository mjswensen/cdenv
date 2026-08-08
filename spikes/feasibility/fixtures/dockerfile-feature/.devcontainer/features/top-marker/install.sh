#!/bin/sh
set -eu
test -f /usr/local/share/cdenv-spike/base-marker
printf '%s\n' "${MESSAGE:-top}" > /usr/local/share/cdenv-spike/top-marker
