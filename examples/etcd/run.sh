#!/usr/bin/env bash

# first argument is the path to shadow
if [ "$#" -ge 1 ]; then
    echo "Prepending $1 to PATH"
    export PATH="$1:${PATH}"
fi

# ANCHOR: body
rm -rf shadow.data; shadow shadow.yaml > shadow.log
cat shadow.data/hosts/*/*.etcdctl.*.stdout
# ANCHOR_END: body
