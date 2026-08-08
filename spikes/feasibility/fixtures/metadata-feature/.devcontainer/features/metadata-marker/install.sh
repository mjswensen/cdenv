#!/bin/sh
set -eu
mkdir -p /usr/local/share/cdenv-spike
printf '%s\n' '#!/bin/sh' 'exec "$@"' > /usr/local/share/cdenv-spike/entrypoint
chmod 0755 /usr/local/share/cdenv-spike/entrypoint
