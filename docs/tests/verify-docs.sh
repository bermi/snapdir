#!/usr/bin/env bash

set -eEuo pipefail
IFS=$'\n\t'

echo "Verifying docs"

echo "Extracting commands from docs/guide.md to ./docs/tests/guide-commands.sh"
{
  echo "#!/usr/bin/env bash"
  echo "# shellcheck disable=SC2086,SC2164"
  echo "alias ls='ls --color=auto'"
  grep "    [^#\`]" ./docs/guide.md | sed 's|^ *||';
} > ./docs/tests/guide-commands.sh
chmod +x ./docs/tests/guide-commands.sh
echo "Running shellckeck on ./docs/tests/guide-commands.sh"
shellcheck ./docs/tests/guide-commands.sh

docker build . -t snapdir-test

# shellcheck disable=SC2016
docker run -it \
  --rm -v "$(pwd)"/docs/tests/guide-commands.sh:/guide.sh  \
  --entrypoint /bin/bash \
  snapdir-test -c "chmod +x guide.sh && bash guide.sh" | \
  sed 's|/tmp/snapdir_[^/]*|$STAGED_DIR|; s|/root|$HOME|' > ./docs/tests/latest-guide-commands.txt

diff ./docs/tests/latest-guide-commands.txt ./docs/tests/expected-guide-commands-expected.txt || {
  echo "ERROR: ./docs/tests/latest-guide-commands.txt and ./docs/tests/expected-guide-commands-expected.txt differ"
  exit 1
}

rm -rf ./docs/tests/latest-*.txt

echo "Documentation tests passed"