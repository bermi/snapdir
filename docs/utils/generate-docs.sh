#!/usr/bin/env bash

set -eEuo pipefail
IFS=$'\n\t'

set -eEuo pipefail

DEFAULT_COMMANDS_REGEXP="snapdir-manifest generate"

_BASE_DIR="$(dirname "${BASH_SOURCE[0]}")"

find_binaries() {
	set -eEuo pipefail
	{
		if [[ "$(uname -s)" == "Darwin" ]]; then
			find . -maxdepth 1 -type f -perm +0111 -print
		else
			find . -maxdepth 1 -perm /u=x,g=x,o=x -type f
		fi
	} | grep -v test | grep -o "snapdir[a-z0-9-]*"
}

get_subcommands() {
	set -eEuo pipefail
  grep -o "local subcommands=\".*\"" | cut -d '"' -f2 | tr '|' '\n' | sort -u | grep -v "^$" | tr '\n' ' ' | sed 's/ $//'
}


get_quick_reference() {
	set -eEuo pipefail
  local contents="$(cat)"
  # local subcommands="generate|cache-id|defaults|flush-cache|test|version"
  # local boolean_args="absolute|cache|debug|no_follow|verbose"
  # local value_required_args="exclude|cache_dir|cache_id|checksum_bin"
  local subcommands="$(echo "$contents" | grep -o "local subcommands=\".*\"" | cut -d '"' -f2 | tr '|' '\n' | grep -v "^$" | tr '\n' ' ')"
}

generate_docs_for_script() {
	set -eEuo pipefail
  local binary="${1:?Missing binary name}"
  local bin_path="$_BASE_DIR/../../$binary"

  test -f "${bin_path}" || {
    echo "Could not find '${bin_path}' file."
    return 1
  }
  local contents
  contents="$(cat "$bin_path")"
  echo "## $binary"
  echo ""
  echo "Usage: $binary [options] [command] [args]"
  echo "Commands: $(echo "$contents" | get_subcommands)"
  echo ""
  local commands
  commands=$(grep -o "^snapdir[a-z_0-9]*" <<<"$contents" | tr '_' '-' | sed -E "s|^$binary-|$binary |")
  for command in $commands; do
    # skip if command matches: run
    if [[ $command =~ ( run)$ ]]; then
      continue
    fi
    local subcommand="${command//$binary /}"
    local command_slug
    command_slug="#$(echo "$command" | tr ' ' '-')"
    local examples
    # Default command?
    if [[ $command =~ ^($DEFAULT_COMMANDS_REGEXP)$ ]]; then
      echo "### $binary (${subcommand})"
      echo ""
      echo "[$binary](#$binary) [${subcommand}]($command_slug) [toc](#snapdir-reference)"
      echo ""
      examples=$(grep "^$binary -" ./docs/tests/tested-commands.sh || echo "")
    else
      echo "### $command"
      echo ""
      echo "[$binary](#$binary) [${subcommand}]($command_slug) [toc](#snapdir-reference)"
      echo ""
      examples=$(grep "^$command" ./docs/tests/tested-commands.sh || echo "")
    fi

    echo ""
    examples="$(echo "$examples" | grep -v "bogus")"
    if [[ $examples != "" ]]; then
      echo "Examples from tests:"
      echo ""
      # shellcheck disable=SC2016
      echo "${examples}" | sort -u | sed -E 's|^(.*)$|```bash\n\1\n```|'
    else
      echo "No examples found on docs/tests/tested-commands.sh"
    fi
    echo ""
  done
}

generate_singe_page_docs() {
	set -eEuo pipefail
	for binary in $(find_binaries | sort); do
    echo ""
    generate_docs_for_script "$binary"
	done
}

generate_toc() {
	set -eEuo pipefail
  grep -E "^#{1,5} " | sed -E 's/(#+) (.+)/\1:\2:\2/g' | awk -F ":" '{ gsub(/#/,"  ",$1); gsub(/[ ]/,"-",$3); print $1 "- [" $2 "](#" tolower($3) ")" }'
}


_docs=""
if test -f "${1:-""}"; then
  _docs=$(generate_docs_for_script "$1" | sed -E 's|^#||')
elif [[ "${1:-""}" == "single-page" ]]; then
  echo "# snapdir reference"
  _docs=$(generate_singe_page_docs)
else
  for binary in $(find_binaries | sort); do
    ./docs/utils/generate-docs.sh "$binary" > "./docs/reference/$binary.md"
  done
  ./docs/utils/generate-docs.sh "single-page" --toc > "./docs/reference.md"
fi

if [[ "$_docs" != "" ]]; then
  # is --toc one of the args?
  if [[ "$*" =~ (--toc) ]]; then
    echo ""
    {
      echo "# Table of contents"
      echo "$_docs"
    } | generate_toc
    echo ""
  fi
  echo "$_docs"
fi
