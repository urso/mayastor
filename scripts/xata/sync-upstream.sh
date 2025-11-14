#!/usr/bin/env bash

# Sync upstream changes for mayastor.
# Wrapper script that sources the generic implementation from utils/dependencies.

SOURCE_REL=$(dirname "$0")/../../utils/dependencies/scripts/xata/sync-upstream.sh

if [ ! -f "$SOURCE_REL" ] && [ -z "$CI" ]; then
  git submodule update --init --recursive
fi

exec "$SOURCE_REL" "$@"
