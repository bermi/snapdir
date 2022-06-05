# snapdir-b2-store

 Snapdir store backed by Backblaze B2.

## Usage

    snapdir-b2-store [OPTIONS] [SUBCOMMAND] [ARGUMENTS]

## Installation

The `snapdir-b2-store` requires the [`b2` command line tool](https://www.backblaze.com/b2/docs/quick_command_line.html) to be installed and available in your `PATH`.

Expose the `snapdir-b2-store` file to a directory in your `PATH` to enabling it on `snapdir`.

## Environment variables

- SNAPDIR_B2_STORE_APPLICATION_KEY: The application key for the B2 storage. Defaults to B2_APPLICATION_KEY.
- SNAPDIR_B2_STORE_APPLICATION_KEY_ID: The application key ID for the B2 storage. Defaults to B2_APPLICATION_KEY_ID.

## Authentication

The b2 store requires authentication before it can be used. You can authenticate by running the following command:

    b2 authorize-account "${SNAPDIR_B2_STORE_APPLICATION_KEY_ID}" "${SNAPDIR_B2_STORE_APPLICATION_KEY}"

## API Reference

### snapdir-b2-store get-push-command

Gets the command for pushing the contents of the staging directory
Backblaze b2.
The staging directory is a temporary directory that is used sync
the contents of a specific manifest to the b2 bucket.
We rely on 'b2 sync' tool to do the actual push and integrity
check.

    snapdir-b2-store get-push-command \
        --staging-dir "${staging_directory}" \
        --store "${store}"

### snapdir-b2-store get-manifest-command

Gets the command for echoing the contents of a manifest given its ID.
This method does not save the manifest on the cache (that's done by
snapdir), it just prints the contents of the manifest.

All

Example:

			snapdir-b2-store get-manifest-command --id "${id}" --store "${store}"

### snapdir-b2-store get-fetch-files-command

Generates the commands required to download from
b2 to the local cache the files defined on a manifest.
Manifests will not exist on the local cache until
all the objects have been fetched.
This function reads the manifest contents from stdin.

Usage:

	cat some_manifest_file | \
      snapdir-b2-store get-fetch-files-command \
      --id="${ID}" \
      --store="b2://bucket-name/long/term/storage/" \
      [--cache-dir="${CACHE_DIR}"]

### snapdir-b2-store get-manifest

Pipes a manifest given its ID to stdout.

Usage:

    snapdir-b2-store get-manifest \
        --id="${ID}" \
        --store="${STORE}" \
        [--retries=5]

### snapdir-b2-store fetch

Performs the actual fetching of files from the remote store.

Usage:

    snapdir-b2-store fetch \
        --store "${STORE}" \
        --checksum="${ID}" \
        --source-path="${SOURCE_FILE_PATH}" \
        --target-path="${REMOTE_FILE_PATH}" \
				  --log-file="${LOG_FILE_PATH}"

### snapdir-b2-store ensure-no-errors

This method is called once all the .objects in the manifest have been
transferred to or from the store.
Errors will be sent to stderr and the process will exit with
a non-zero status.

Usage:

    snapdir-b2-store verify-transactions \
        --checksum "aa91e498f401ea9e6ddbaa1138a0dbeb030fab8defc1252d80c77ebefafbc70d" \
        --log-file "/log/file/for/the/transaction"

### snapdir-b2-store test

Runs the tests for the b2 store

Usage:

    snapdir-b2-store test --store="${STORE}"

Environment variables:

- SNAPDIR_B2_STORE_APPLICATION_KEY
