## Guide

This is a continuation of the [guide under docs/](docs/guide.md) for using
Backblaze B2.

Make sure you install the b2cli tool.

    which b2 >/dev/null || {
        wget -q "https://github.com/Backblaze/B2_Command_Line_Tool/releases/latest/checkout/b2-linux" | sudo tee "/usr/local/bin/b2" >/dev/null
        sudo chmod +x /usr/local/bin/b2
    }

Authenticate by exposing SNAPDIR_B2_STORE_APPLICATION_KEY_ID SNAPDIR_B2_STORE_APPLICATION_KEY in
your environment and then run the following command:

    b2 authorize-account "${SNAPDIR_B2_STORE_APPLICATION_KEY_ID}" "${SNAPDIR_B2_STORE_APPLICATION_KEY}"

Create a bucket with the name "snapdir-snapdir-guide". We recommend that you
setup a bucket policy that prevents files from being deleted. Since you
might choose a different name for your bucket, we'll save the store as
an environment variable for the rest of the snapdir-guide.

    SNAPDIR_B2_STORE_BUCKET_NAME=snapdir-snapdir-guide

We will now push the contents of `snapdir-guide` to the store repository.

    snapdir push --store "b2://${SNAPDIR_B2_STORE_BUCKET_NAME}/snapdir-guide" snapdir-guide
    # Outputs: df4b3a7b6c04e5b14ebb548a28ac0dea6c645f0ecfde85df2c0911ac10d2e8a9

If you run into issues, you can use the `--verbose` and `--debug`
options to get more information about the push.

Let's clear our local cache and verify that we can pull the snapdir from
the store repository.

    rm -rf ${HOME}/.cache/snapdir snapdir-guide && \
    snapdir pull --id=df4b3a7b6c04e5b14ebb548a28ac0dea6c645f0ecfde85df2c0911ac10d2e8a9 --store "b2://${SNAPDIR_B2_STORE_BUCKET_NAME}/snapdir-guide" snapdir-guide

## Pushing all snapshots

We can now make sure that all the local manifests exist on the store by
calling:

We can now push snapshots off-process.

## Push a --id=df4b3a7b6c04e5b14ebb548a28ac0dea6c645f0ecfde85df2c0911ac10d2e8a9

```bash
#!/bin/bash

set -eEuo pipefail

# Has ./snapdir changed?
if git diff --name-only HEAD | grep -q snapdir; then
  # lint
  shellcheck ./snapdir
  git diff --exit-code -- ./snapdir

  # format
  shfmt -w -s ./snapdir
  git diff --exit-code -- ./snapdir

  # test
  ./snapdir test
fi
```

