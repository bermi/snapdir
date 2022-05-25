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
  } | grep -v test | grep -o "snapdir[a-z0-9-]*"
}

show_public_api_methods() {
  set -eEuo pipefail
  local default_commands_regexp="snapdir-manifest generate"
  for binary in $(find_binaries | sort); do
    local contents
    contents="$(cat "$binary")"
    echo ""
    echo "## $binary"
    echo ""
    local commands
    commands=$(grep -o "^snapdir[a-z_0-9]*" <<<"$contents" | tr '_' '-' | sed -E "s|^$binary-|$binary |")
    for command in $commands; do
      # skip if command matches: run
      if [[ "$command" =~ ^(run)$ ]]; then
        continue
      fi
      local examples
      # Default command?
      if [[ "$command" =~ ^($default_commands_regexp)$ ]]; then
        echo "### $binary [${command//$binary /}]"
        examples=$(grep "^$binary -" ./docs/tests/tested-commands.sh || echo "")
      else
        echo "### $command"
        examples=$(grep "^$command" ./docs/tests/tested-commands.sh || echo "")
      fi

      echo ""
      if [[ "$examples" != "" ]]; then
        echo "Examples from tests:"
        echo ""
        # shellcheck disable=SC2016
        echo "${examples}" | sort -u | sed -E 's|^(.*)$|```bash\n\1\n```|'
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