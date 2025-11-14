#!/usr/bin/env bash

# Update xataio submodules for mayastor.
# Wrapper script that sources the generic implementation from utils/dependencies.

SOURCE_REL=$(dirname "$0")/../../utils/dependencies/scripts/xata/update-submodules.sh

if [ ! -f "$SOURCE_REL" ] && [ -z "$CI" ]; then
  git submodule update --init --recursive
fi

. "$SOURCE_REL"

update_xata_submodules "$@"
