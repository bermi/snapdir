#!/usr/bin/env bash

set -eEuo pipefail
IFS=$'\n\t'

echo "Verifying docs"

echo "Extracting commands from docs/guide.md to ./docs/tests/guide-commands.sh"
{
  echo "#!/usr/bin/env bash"
  echo "# shellcheck disable=SC2086,SC2164"
  grep "    [^#\`]" ./docs/guide.md | sed 's|^ *||';
} > ./docs/tests/guide-commands.sh
chmod +x ./docs/tests/guide-commands.sh
echo "Running shellckeck on ./docs/tests/guide-commands.sh"
shellcheck ./docs/tests/guide-commands.sh

echo "Building docker image"
docker build . -t snapdir-test

echo "Running documentation commands"
# shellcheck disable=SC2016
docker run \
  --rm -v "$(pwd)"/docs/tests/guide-commands.sh:/root/guide.sh  \
  --entrypoint /bin/bash \
  --workdir /root \
  snapdir-test -c "set -eEuo pipefail && chmod +x guide.sh && ./guide.sh" 2>&1 | \
  tr -d $'\r' | sed 's|/tmp/snapdir_[^/]*|${STAGED_DIR}|g; s|/root|${HOME}|g' > ./docs/tests/latest-guide-commands.txt

echo "Making the output of the commands is shown on docs/guide.md"
# for each line on ./docs/tests/latest-guide-commands.txt make sure it is found in ./docs/guide.md
while read -r line; do
  if ! grep -q "$line" ./docs/guide.md; then
    echo "ERROR: '$line' Output not found in ./docs/guide.md"
    exit 1
  fi
done < ./docs/tests/latest-guide-commands.txt
echo "outputs verified"

test -f ./docs/tests/expected-guide-commands.txt || {
  cp ./docs/tests/latest-guide-commands.txt ./docs/tests/expected-guide-commands.txt
}
echo "comparing to a known version ./docs/tests/expected-guide-commands.txt"
diff <(sort ./docs/tests/latest-guide-commands.txt) <(sort ./docs/tests/expected-guide-commands.txt) || {
  echo "ERROR: ./docs/tests/latest-guide-commands.txt and ./docs/tests/expected-guide-commands.txt differ"
  exit 1
}

rm -rf ./docs/tests/latest-*.txt

echo "Documentation tests passed"