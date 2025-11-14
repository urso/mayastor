#!/usr/bin/env bash

# Create and push multi-platform Docker manifests
# Wrapper script that sources the generic implementation from utils/dependencies.

SOURCE_REL=$(dirname "$0")/../../utils/dependencies/scripts/xata/create-manifest.sh

if [ ! -f "$SOURCE_REL" ] && [ -z "$CI" ]; then
  git submodule update --init --recursive
fi

. "$SOURCE_REL"

create_manifests $@
