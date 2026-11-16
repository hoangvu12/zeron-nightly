#!/usr/bin/env bash
# The live end-to-end run of a chat on Docker Sandbox (spec #37), on a Linux
# box: crates/engine/tests/docker_live.rs against a real Docker account and
# the real edge. Costs a few cents: one small Blank sandbox, deleted at the end.
#
#   ZERON_LIVE_HOST_DIR=<data dir signed in with `zeron-engine login`> \
#   DOCKER_TOKEN_FILE=<file holding a Docker access token> \
#   [ZERON_SANDBOX_ENGINE_TARBALL=<url of this branch's engine tarball>] \
#   scripts/live-docker-sandbox.sh
#
# The token is read from the file, never passed on the command line or
# printed. A borrowed `sbx` access token lasts 15 minutes; the run takes
# about 10.
set -euo pipefail
: "${ZERON_LIVE_HOST_DIR:?the signed-in data dir of the host engine}"
: "${DOCKER_TOKEN_FILE:?a file with a Docker access token}"
ZERON_DOCKER_SMOKE_TOKEN=$(cat "$DOCKER_TOKEN_FILE")
export ZERON_DOCKER_SMOKE_TOKEN ZERON_LIVE_HOST_DIR
exec cargo test --locked -p zeron-engine --test docker_live -- --ignored --nocapture
