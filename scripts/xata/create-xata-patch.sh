#!/usr/bin/env bash

# Create xata patch tag for mayastor.
# Wrapper script that sources the generic implementation from utils/dependencies.

SOURCE_REL=$(dirname "$0")/../../utils/dependencies/scripts/xata/create-xata-patch.sh

if [ ! -f "$SOURCE_REL" ] && [ -z "$CI" ]; then
  git submodule update --init --recursive
fi

exec "$SOURCE_REL" "$@"
