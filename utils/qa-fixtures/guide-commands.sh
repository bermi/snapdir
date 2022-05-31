#!/usr/bin/env bash
# shellcheck disable=SC2086,SC2164
umask 077
mkdir -p ~/snapdir-guide/example/
cd ~/snapdir-guide/
touch example/{foo,bar}.txt
snapdir manifest example
b3sum --no-names example/* | sort -u | tr -d '\n' | b3sum  --no-names
snapdir manifest example | b3sum --no-names
snapdir id example
STAGED_DIR=$(snapdir stage example --keep | tee /dev/stderr)
find ${STAGED_DIR} ! -type d
readlink -f ${STAGED_DIR}/.objects/af1/349/b9f/5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262
rm -rf "${STAGED_DIR}"
echo "foo" > example/foo.txt
snapdir stage example
snapdir id example
cat ${HOME}/.cache/snapdir/.manifests/8af/03a/1be/c09b1838d2c4f56c6940ed35ccdad1064243d2d775e8347ba82b9be
b3sum --no-names ${HOME}/.cache/snapdir/.manifests/8af/03a/1be/c09b1838d2c4f56c6940ed35ccdad1064243d2d775e8347ba82b9be
rm -rf example
snapdir checkout --id=8af03a1bec09b1838d2c4f56c6940ed35ccdad1064243d2d775e8347ba82b9be example
cat example/foo.txt
snapdir checkout --id=c678a299380893769bd7795628b96147229b410a9d5a5b7cae563bcae3c27857 example
echo "bar" > example/bar.txt
snapdir stage example
snapdir verify --verbose --id df4b3a7b6c04e5b14ebb548a28ac0dea6c645f0ecfde85df2c0911ac10d2e8a9
echo "tampered" > ${HOME}/.cache/snapdir/.objects/b31/99d/36d/434044e6778b77d13f8dbaba32a73d9522c1ae8d0f73ef1ff14e71f
snapdir verify --verbose --id df4b3a7b6c04e5b14ebb548a28ac0dea6c645f0ecfde85df2c0911ac10d2e8a9
snapdir verify --purge --id df4b3a7b6c04e5b14ebb548a28ac0dea6c645f0ecfde85df2c0911ac10d2e8a9
snapdir stage example
snapdir push --store "file://${HOME}/snapdir-guide/data" example
rm -rf ${HOME}/.cache/snapdir example
snapdir pull --id=df4b3a7b6c04e5b14ebb548a28ac0dea6c645f0ecfde85df2c0911ac10d2e8a9 --store "file://${HOME}/snapdir-guide/data" example
cat example/{foo,bar}.txt
rm -rf ${HOME}/.cache/snapdir
snapdir fetch --id=df4b3a7b6c04e5b14ebb548a28ac0dea6c645f0ecfde85df2c0911ac10d2e8a9 --store "file://${HOME}/snapdir-guide/data"
rm -rf ${HOME}/.cache/snapdir ~/snapdir-guide/
