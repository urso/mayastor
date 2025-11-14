#!/usr/bin/env bash

# Build and push mayastor control plane docker images to AWS ECR.
# Wrapper script that sources the generic implementation from utils/dependencies.

SOURCE_REL=$(dirname "$0")/../../utils/dependencies/scripts/xata/build-and-push-ecr.sh

if [ ! -f "$SOURCE_REL" ] && [ -z "$CI" ]; then
  git submodule update --init --recursive
fi

. "$SOURCE_REL"

build_and_push_ecr $@
