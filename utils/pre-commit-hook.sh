#!/usr/bin/env bash

set -eEuo pipefail
IFS=$'\n\t'

echo "$SNAPDIR_BIN_FILES" | xargs shellcheck
echo "$SNAPDIR_BIN_FILES" | xargs shfmt -w -s

# Run for every snapdir file that's been changed
for script in $(git diff --name-only HEAD | grep "^snapdir" | grep -v ".md"); do
  echo "Running $script"
  # lint
  # is ./"$script" a shell script?
  if head -1 ./"$script" | grep -q "bash"; then
    shellcheck ./"$script"
  else
    echo "Skipping $script, not a shell script"
  fi
  git diff --exit-code -- ./"$script" || {
    echo "'./$script' has changes that have not been staged. Please stage or stash them." >&2
    exit 1
  }

  # format
  if head -1 ./"$script" | grep -q "bash"; then
    shfmt -w -s ./"$script"
  else
    echo "Skipping $script, not a shell script"
  fi
  git diff --exit-code -- ./"$script" || {
    echo "'./$script' has been reformatted by shfmt. Please review the changes and stage them." >&2
    exit 1
  }

done

export ENVIRONMENT=test
unset _SNAPDIR_RUN_LOG_PATH
./snapdir-test integration
