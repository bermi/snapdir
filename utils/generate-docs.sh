#!/usr/bin/env bash

set -eEuo pipefail
IFS=$'\n\t'

DEBUG=${DEBUG:-true}
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

_get_local_values() {
  # Extracts the value of a variable with a "string|separator" format
	set -eEuo pipefail
  local key="$1"
  grep -o "local $key=\"[^\"]*|[^\"]*\"" | cut -d '"' -f2 | tr '|' '\n' | sort -u | grep -v "^$"
}

get_commands() {
	set -eEuo pipefail
  _get_local_values "commands"
}
get_options() {
	set -eEuo pipefail
  _get_local_values "boolean_args" | tr '_' '-'
}
get_parametrized_options() {
	set -eEuo pipefail
  _get_local_values "value_required_args" | tr '_' '-'
}

get_global_options() {
	set -eEuo pipefail
  get_options
}

_get_examples_from_tests() {
  set -eEuo pipefail
  local command="$1"
  local binary="$2"
  local examples
  # Default command?
  if [[ $command =~ ^($DEFAULT_COMMANDS_REGEXP)$ ]]; then
    examples=$(grep "^$binary -" ./utils/qa-fixtures/tested-commands.sh || echo "")
  else
    examples=$(grep "^$command" ./utils/qa-fixtures/tested-commands.sh || echo "")
  fi
  examples="$(echo "$examples" | grep -v "bogus")"

  if [[ $examples != "" ]]; then
    # shellcheck disable=SC2016
    echo "${examples}" | sort -u
  fi
  #  | sed -E 's|^(.*)$|```bash\n\1\n```|'
}

_group_param_options() {
  set -eEuo pipefail
  local options
  options="$(cat)"
  local result=""
  for option in $options; do
    local key
    key="$(echo "$option" | cut -d '=' -f1)"
    local value
    value="$(echo "$option" | cut -d '=' -f2)"
    if grep -E -q "(^| )$key=" <<<"$result"; then
      result="${result},$value"
    else
      result="${result} $key=$value"
    fi
  done
  echo "$result" | sed -E 's/^ //' | tr ' ' '\n'
}

_get_or_list() {
  tr '\n' '|' | sed -e 's/|$//'
}

_get_global_usage() {
  set -eEuo pipefail
  echo "## $binary"
  echo ""
  echo "Auto-generated Usage:"
  echo ""
  echo "    $binary [options] [command] [args]"
  echo ""
  echo "Commands"
  echo ""
  echo "    $(echo "$contents" | get_commands | _get_or_list)"
  echo ""
  echo "Options"
  echo ""
  echo "    --($valid_options)"
  echo ""
  echo "Params"
  echo ""
  echo "    --($valid_params)=<value>"
  echo ""
  echo ""
}

_get_method_usage() {
	set -eEuo pipefail
  local usage_command="$1"
  local examples_from_tests="$2"
  local valid_options="$3"
  local valid_params="$4"

  # we populate options from what's been tested only
  local command_options
  command_options=$(echo "$examples_from_tests" | grep -E -o "\-\-($valid_options)" | sed 's/--//' | sort -u | _get_or_list || echo "")
  local param_options
  param_options=$(echo "$examples_from_tests" | grep -E -o "\-\-($valid_params)[ =][^ ]*" | sed 's/--//' | sort -u | tr ' ' '=' | _group_param_options | _get_or_list || echo "")
  local positional_args
  positional_args=$(echo "$examples_from_tests" | grep -E -o "[\"] \"[^\"]*\"$" | cut -d' ' -f2 | sort -u | _get_or_list || echo "")

  echo "Auto-generated Usage:"
  echo ""
  echo -n "    ${usage_command}"
  if [[ $command_options != "" ]]; then
    echo " \\"
    echo -n "        --($command_options)"
  fi
  if [[ $param_options != "" ]]; then
    echo " \\"
    echo -n "        --($param_options)"
  fi
  if [[ $positional_args != "" ]]; then
    echo " \\"
    echo -n "        $positional_args"
  fi

  echo ""

}

generate_docs_for_script() {
	set -eEuo pipefail
  local binary="${1:?Missing binary name}"
  local bin_path="$_BASE_DIR/../$binary"

  test -f "${bin_path}" || {
    echo "Could not find '${bin_path}' file."
    return 1
  }
  local contents
  contents="$(cat "$bin_path")"
  local valid_options
  valid_options="$(echo "$contents" | get_options | _get_or_list || echo "")"
  local valid_params
  valid_params="$(echo "$contents" | get_parametrized_options | _get_or_list || echo "")"

  if [[ "${DEBUG}" == "true" ]]; then
    _get_global_usage
  fi

  "$bin_path" --help

  local commands
  commands=$(grep -o "^snapdir[a-z_0-9]*" <<<"$contents" | tr '_' '-' | sed -E "s|^$binary-|$binary |" || echo "")

  if [[ "$commands" == "" ]]; then
    return 0
  fi

  echo "## API Reference"
  echo ""

  for command in $commands; do
    # local command_slug
    # if [[ "$command" != "snapdir checkout" ]]; then
    #   continue;
    # fi
    # command_slug="#$(echo "$command" | tr ' ' '-')"
    local subcommand
    subcommand="${command//$binary /}"

    local usage_command="$command"
    # Default command?
    if [[ $command =~ ^($DEFAULT_COMMANDS_REGEXP)$ ]]; then
      echo "### $binary"
      echo ""
      usage_command="$binary"
      echo "Default command. Alias for: ${binary} ${subcommand}"
    else
      echo "### $command"
    fi

    echo ""
    # echo "[$binary](#$binary) [${subcommand}]($command_slug)"
    # echo ""

    local inline_docs
    inline_docs="$(./"$binary" "$subcommand" --help 2>&1 || echo "")"

    if [[ "${inline_docs:-""}" == "" ]]; then
      echo "ERROR: Missing inline docs for '$command'"
    else
      if [[ "${DEBUG}" == "true" ]]; then
        echo "### Inline documentation"
        echo ""
      fi
      echo "$inline_docs"
    fi

    if [[ "${DEBUG}" == "true" ]]; then
      local examples_from_tests
      examples_from_tests=$(_get_examples_from_tests "$command" "$binary")
      echo ""
      _get_method_usage "$usage_command" "$examples_from_tests" "$valid_options" "$valid_params"

      if [[ $examples_from_tests != "" ]]; then
        echo "### Examples from tests"
        echo ""
        # shellcheck disable=SC2016
        echo "${examples_from_tests}" | sed -E 's|^(.*)$|```bash\n\1\n```|'
      else
        echo "ERROR: No examples found on utils/qa-fixtures/tested-commands.sh"
      fi
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
  _docs=$(generate_docs_for_script "$1")
elif [[ "${1:-""}" == "single-page" ]]; then
  echo "# snapdir reference"
  _docs=$(generate_singe_page_docs)
else
  mkdir -p tmp/reference
  for binary in $(find_binaries | sort); do
    ./utils/generate-docs.sh "$binary" > "./tmp/reference/$binary.md"
  done
  ./utils/generate-docs.sh "single-page" --toc > "./tmp/reference.md"
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
