#!/usr/bin/env bash

set -eEuo pipefail
IFS=$'\n\t'

which docker > /dev/null || {
  echo "Docker is not installed. We need docker to verify the docs on a pristine environment." >&2
  exit 1
}

echo "Verifying docs"

echo "Extracting commands from docs/guide.md to ./utils/qa-fixtures/guide-commands.sh"
{
  echo "#!/usr/bin/env bash"
  echo "# shellcheck disable=SC2086,SC2164"
  grep "    [^#\`]" ./docs/guide.md | sed 's|^ *||';
} > ./utils/qa-fixtures/guide-commands.sh
chmod +x ./utils/qa-fixtures/guide-commands.sh
echo "Running shellckeck on ./utils/qa-fixtures/guide-commands.sh"
shellcheck ./utils/qa-fixtures/guide-commands.sh

echo "Building docker image"
docker build . -t snapdir-test

echo "Running documentation commands"
# shellcheck disable=SC2016
docker run \
  --rm -v "$(pwd)"/utils/qa-fixtures/guide-commands.sh:/root/guide.sh  \
  --entrypoint /bin/bash \
  --workdir /root \
  snapdir-test -c "set -eEuo pipefail && chmod +x guide.sh && ./guide.sh" 2>&1 | \
  tr -d $'\r' | sed 's|/tmp/snapdir_[^/]*|${STAGED_DIR}|g; s|/root|${HOME}|g' > ./utils/qa-fixtures/latest-guide-commands.txt

echo "Making the output of the commands is shown on docs/guide.md"
# for each line on ./utils/qa-fixtures/latest-guide-commands.txt make sure it is found in ./docs/guide.md
while read -r line; do
  if ! grep -q "$line" ./docs/guide.md; then
    echo "ERROR: '$line' Output not found in ./docs/guide.md"
    exit 1
  fi
done < ./utils/qa-fixtures/latest-guide-commands.txt
echo "outputs verified"

test -f ./utils/qa-fixtures/expected-guide-commands.txt || {
  cp ./utils/qa-fixtures/latest-guide-commands.txt ./utils/qa-fixtures/expected-guide-commands.txt
}
echo "comparing to a known version ./utils/qa-fixtures/expected-guide-commands.txt"
diff <(sort ./utils/qa-fixtures/latest-guide-commands.txt) <(sort ./utils/qa-fixtures/expected-guide-commands.txt) || {
  echo "ERROR: ./utils/qa-fixtures/latest-guide-commands.txt and ./utils/qa-fixtures/expected-guide-commands.txt differ"
  exit 1
}

rm -rf ./utils/qa-fixtures/latest-*.txt

echo "Documentation tests passed"