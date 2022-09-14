#!/usr/bin/env bash

set -eEuo pipefail
IFS=$'\n\t'

command -v wget >/dev/null 2>&1 || { echo >&2 "Please install wget to download snapdir."; exit 1; }

for script in snapdir snapdir-manifest snapdir-s3-store snapdir-file-store snapdir-b2-store snapdir-sqlite3-catalog snapdir-test; do
    wget -q -4 -p "https://raw.githubusercontent.com/bermi/snapdir/main/${script}" -O "$script"
    chmod +x "$script"
done

mv snapdir* /usr/local/bin/ || {
    echo "It looks like you don't have write permissions to /usr/local/bin" >&2
    echo "" >&2
    echo "Please run:" >&2
    echo "" >&2
    echo "  sudo mv snapdir* /usr/local/bin/" >&2
    echo "" >&2
    echo "to complete the setup" >&2
    echo "" >&2
    exit 1
}

command -v b3sum 2> /dev/null 1>/dev/null || {
  echo "b3sum is not installed. Please install it with" >&2
  echo "Debian-flavored Linux: sudo apt install b3sum" >&2
  echo "MacOS: brew install b3sum" >&2
}

command -v sqlite3 2> /dev/null 1>/dev/null || {
  echo "sqlite3 is not installed. Please install it with" >&2
  echo "Debian-flavored Linux: sudo apt install sqlite3" >&2
  echo "MacOS: brew install sqlite3" >&2
}

echo "Installation complete."

echo "Please run 'snapdir help' to get started."