# Snapdir b2 store

The `snapdir-b2-store` provides support for using Backblaze's B2 storage.

It requires the [`b2` command line tool](https://www.backblaze.com/b2/docs/quick_command_line.html) to be installed and available in your `PATH`.

## Installation

Copy the `snapdir-b2-store` file to a directory in your `PATH`.

## Environment variables

- SNAPDIR_B2_STORE_APPLICATION_KEY: The application key for the B2 storage. Defaults to B2_APPLICATION_KEY.
- SNAPDIR_B2_STORE_APPLICATION_KEY_ID: The application key ID for the B2 storage. Defaults to B2_APPLICATION_KEY_ID.

## Authentication

The b2 store requires authentication before it can be used. You can authenticate by running the following command:

    b2 authorize-account "${SNAPDIR_B2_STORE_APPLICATION_KEY_ID}" "${SNAPDIR_B2_STORE_APPLICATION_KEY}"

## Guide

This is a continuation of the [guide under docs/guide.md](./guide.md) for using
Backblaze B2.

Make sure you install the b2cli tool.

    b2 version || {
        wget -q "https://github.com/Backblaze/B2_Command_Line_Tool/releases/latest/checkout/b2-linux" | sudo tee "/usr/local/bin/b2" >/dev/null
        sudo chmod +x /usr/local/bin/b2
    }

Authenticate by exposing SNAPDIR_B2_STORE_APPLICATION_KEY_ID SNAPDIR_B2_STORE_APPLICATION_KEY in
your environment and then run the following command:

    b2 authorize-account "${SNAPDIR_B2_STORE_APPLICATION_KEY_ID}" "${SNAPDIR_B2_STORE_APPLICATION_KEY}"

Create a bucket with the name "snapdir-example". We recommend that you
setup a bucket policy that prevents files from being deleted. Since you
might choose a different name for your bucket, we'll save the store as
an environment variable for the rest of the example.

    SNAPDIR_B2_STORE_BUCKET_NAME=snapdir-example

We will now push the contents of `example` to the store repository.

    snapdir push --store "b2://${SNAPDIR_B2_STORE_BUCKET_NAME}/example" example
    # Outputs: df4b3a7b6c04e5b14ebb548a28ac0dea6c645f0ecfde85df2c0911ac10d2e8a9

If you run into issues, you can use the `--verbose` and `--debug`
options to get more information about the push.

Let's clear our local cache and verify that we can pull the snapdir from
the store repository.

    rm -rf ${HOME}/.cache/snapdir example && \
    snapdir pull --id=df4b3a7b6c04e5b14ebb548a28ac0dea6c645f0ecfde85df2c0911ac10d2e8a9 --store "b2://${SNAPDIR_B2_STORE_BUCKET_NAME}/example" example

