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
			find . -maxdepth 1 -perm /u=x,g=x,o=x -type f
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
			if [[ $command =~ ( run)$ ]]; then
				continue
			fi
      local subcommand="${command//$binary /}"
      local subcommand_slug
      subcommand_slug="#$(echo "$subcommand" | tr ' ' '-')"
			local examples
			# Default command?
			if [[ $command =~ ^($default_commands_regexp)$ ]]; then
				echo "### $binary (${subcommand})"
        echo ""
        echo "<div><span class=command>[$binary](#$binary)</span> <span class=\"subcommand optional\">[${subcommand}]($subcommand_slug)</span> <span class=toc>[toc](#snapdir-reference)</span></div>"
        echo ""
				examples=$(grep "^$binary -" ./docs/tests/tested-commands.sh || echo "")
			else
				echo "### $command"
        echo ""
        echo "<div><span class=command>[$binary](#$binary)</span> <span class=subcommand>[${subcommand}]($subcommand_slug)</span> <span class=toc>[toc](#snapdir-reference)</span></div>"
        echo ""
        examples=$(grep "^$command" ./docs/tests/tested-commands.sh || echo "")
			fi

			echo ""
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
	done
}

_docs=$(show_public_api_methods)

echo "# snapdir reference"
echo ""

{
	echo "# Table of contents"
	echo "$_docs"
} | grep -E "^#{1,5} " | sed -E 's/(#+) (.+)/\1:\2:\2/g' | awk -F ":" '{ gsub(/#/,"  ",$1); gsub(/[ ]/,"-",$3); print $1 "- [" $2 "](#" tolower($3) ")" }'

echo ""
echo "$_docs"

# find . -maxdepth 1 -perm /u=x,g=x,o=x  -type f | xargs cat | grep -o "^snapdir[a-z_0-9]*" | tr '_' '-'
