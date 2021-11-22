#!/bin/bash
# This script replaces the version of Cargo on the server with the version of Cargo being built

if [[ ! -f "Cargo.toml" ]]; then
    echo "Must be run from root of project."
    exit 1
fi

cargo install --path . --force
