#!/bin/sh
# disktree-web container entrypoint: the token story lives here, not in the
# image, so the defaults are safe no matter where the port ends up published.
#
#   DISKTREE_TOKEN unset   → a random one is generated and printed to the
#                            container logs (docker logs disktree-web)
#   DISKTREE_TOKEN=secret  → that one
#   DISKTREE_TOKEN=none    → no token, for loopback-only demos
set -eu

case "${DISKTREE_TOKEN:-}" in
    none)
        # The binary reads the same variable; the opt-out word must not
        # become the token.
        unset DISKTREE_TOKEN
        exec disktree-web --listen 0.0.0.0:8737 "$@"
        ;;
    "")
        token=$(head -c 12 /dev/urandom | od -An -vtx1 | tr -d ' \n')
        echo "disktree-web: generated a token for this container:" >&2
        echo "disktree-web:   http://<host>:8737/?token=$token" >&2
        exec disktree-web --listen 0.0.0.0:8737 --token "$token" "$@"
        ;;
    *)
        exec disktree-web --listen 0.0.0.0:8737 --token "$DISKTREE_TOKEN" "$@"
        ;;
esac
