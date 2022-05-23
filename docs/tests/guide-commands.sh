#!/usr/bin/env bash
# shellcheck disable=SC2086,SC2164
alias ls='ls --color=auto'
cd ~
mkdir -p snapdir-guide/
umask 077 snapdir-guide/
touch snapdir-guide/{foo,bar}.txt
snapdir manifest snapdir-guide
b3sum --no-names snapdir-guide/* | sort -u | tr -d '\n' | b3sum  --no-names
snapdir manifest snapdir-guide | b3sum --no-names
snapdir id snapdir-guide
STAGED_DIR=$(snapdir stage snapdir-guide --keep | tee /dev/stderr)
find $STAGED_DIR ! -type d
readlink -f $STAGED_DIR/.objects/af1/349/b9f/5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262
rm -rf "$STAGED_DIR"
echo "foo" > snapdir-guide/foo.txt
snapdir stage snapdir-guide
snapdir id snapdir-guide
cat ${HOME}/.cache/snapdir/.manifests/f0b/8a6/7f5/fb5ddd6d67aa9ae5f843d9b00793a68d8d79235834b0b974abe904f
b3sum --no-names ${HOME}/.cache/snapdir/.manifests/f0b/8a6/7f5/fb5ddd6d67aa9ae5f843d9b00793a68d8d79235834b0b974abe904f
rm -rf snapdir-guide
snapdir checkout --id=f0b8a67f5fb5ddd6d67aa9ae5f843d9b00793a68d8d79235834b0b974abe904f snapdir-guide
cat snapdir-guide/{foo,bar}.txt
snapdir checkout --id=0e10f2cc09efcb1a4b9bbf61eeac6c29494c5b2fa556496d984c7a5b157c5e2e snapdir-guide
echo "bar" > snapdir-guide/bar.txt
snapdir stage snapdir-guide
snapdir verify --verbose --id df4b3a7b6c04e5b14ebb548a28ac0dea6c645f0ecfde85df2c0911ac10d2e8a9
echo "tampered" > ${HOME}/.cache/snapdir/.objects/b31/99d/36d/434044e6778b77d13f8dbaba32a73d9522c1ae8d0f73ef1ff14e71f
snapdir verify --verbose --id df4b3a7b6c04e5b14ebb548a28ac0dea6c645f0ecfde85df2c0911ac10d2e8a9
snapdir stage snapdir-guide
snapdir push --store "file://${HOME}/snapdir-data" snapdir-guide
rm -rf ${HOME}/.cache/snapdir snapdir-guide
snapdir pull --verbose --id=df4b3a7b6c04e5b14ebb548a28ac0dea6c645f0ecfde85df2c0911ac10d2e8a9 --store "file://${HOME}/snapdir-data" snapdir-guide
cat snapdir-guide/{foo,bar}.txt
rm -rf ${HOME}/.cache/snapdir
snapdir fetch --id=df4b3a7b6c04e5b14ebb548a28ac0dea6c645f0ecfde85df2c0911ac10d2e8a9 --store "file://${HOME}/snapdir-data"
