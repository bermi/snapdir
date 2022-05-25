#!/usr/bin/env bash

set -eEuo pipefail
IFS=$'\n\t'

set -eEuo pipefail

find_binaries() {
  set -eEuo pipefail
  {
    if [[ "$(uname -s)" == "Darwin" ]]; then
      find . -maxdepth 1 -type f -perm +0111 -print
    else
      find . -maxdepth 1 -perm /u=x,g=x,o=x  -type f
    fi
  } | grep -o "snapdir[a-z0-9-]*"
}

show_public_api_methods() {
  set -eEuo pipefail
  for binary in $(find_binaries | sort); do
    local contents
    contents="$(cat "$binary")"
    echo ""
    echo "## $binary"
    echo ""
    local commands
    commands=$(grep -o "^snapdir[a-z_0-9]*" <<<"$contents" | tr '_' '-' | sed -E "s|^$binary-|$binary |")
    for command in $commands; do
      echo "### $command"
      local examples
      examples=$(grep "^$command" ./docs/tests/tested-commands.sh || echo "")
      echo ""
      if [[ "$examples" != "" ]]; then
        echo "Examples from tests:"
        echo ""
        echo "${examples}" | sed 's|^|    |' | sort -u
      else
        echo "No examples found on docs/tests/tested-commands.sh"
      fi
      echo ""
    done
  done
}

echo "# snapdir reference
"
show_public_api_methods

# find . -maxdepth 1 -perm /u=x,g=x,o=x  -type f | xargs cat | grep -o "^snapdir[a-z_0-9]*" | tr '_' '-'