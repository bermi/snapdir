# snapdir-gcs-store

Snapdir store backed by Google Cloud Storage.

## Usage

    snapdir-gcs-store [OPTIONS] [SUBCOMMAND] [ARGUMENTS]

## Installation

The `snapdir-gcs-store` requires the [`gcloud` command line tool](https://cloud.google.com/sdk/docs/install) to be installed and available in your `PATH`.

Expose the `snapdir-gcs-store` file to a directory in your `PATH` to enabling it on `snapdir`.

## Environment variables

- SNAPDIR_GCS_STORE_CREDENTIALS_FILE: Path to the Google Cloud credentials file. Defaults to GOOGLE_APPLICATION_CREDENTIALS.

## Authentication

Check your authentication with the command:

    gcloud auth list

If you encounter issues, run `gcloud auth login` to authenticate.

## API Reference

### snapdir-gcs-store get-push-command

Gets the command for syncing the contents of the staging directory
to Google Cloud Storage.
The staging directory is a temporary directory that is used sync
the contents of a specific manifest to the GCS bucket.
We rely on 'gcloud storage' to do the actual push and integrity
check.

    snapdir-gcs-store get-push-command \
        --staging-dir "${staging_directory}" \
        --store "${store}"

### snapdir-gcs-store get-manifest-command

Gets the command for echoing the contents of a manifest given its ID.
This method does not save the manifest on the cache (that's done by
snapdir), it just prints the contents of the manifest.

Example:

			snapdir-gcs-store get-manifest-command --id "${id}" --store "${store}"

### snapdir-gcs-store get-fetch-files-command

Generates the commands required to download from
GCS to the local cache the files defined on a manifest.
Manifests will not exist on the local cache until
all the objects have been fetched.
This function reads the manifest contents from stdin.

Usage:

	cat some_manifest_file | \
      snapdir-gcs-store get-fetch-files-command \
      --id="${ID}" \
      --store="gs://bucket-name/long/term/storage/" \
      [--cache-dir="${CACHE_DIR}"]

### snapdir-gcs-store get-manifest

Pipes a manifest given its ID to stdout.

Usage:

    snapdir-gcs-store get-manifest \
        --id="${ID}" \
        --store="${STORE}" \
        [--retries=5]

### snapdir-gcs-store fetch

Performs the actual fetching of files from the remote store.

Usage:

    snapdir-gcs-store fetch \
        --store "${STORE}" \
        --checksum="${ID}" \
        --source-path="${SOURCE_FILE_PATH}" \
        --target-path="${REMOTE_FILE_PATH}" \
        --log-file="${LOG_FILE_PATH}"

### snapdir-gcs-store ensure-no-errors

This method is called once all the .objects in the manifest have been
transferred to or from the store.
Errors will be sent to stderr and the process will exit with
a non-zero status.

Usage:

    snapdir-gcs-store verify-transactions \
        --checksum "aa91e498f401ea9e6ddbaa1138a0dbeb030fab8defc1252d80c77ebefafbc70d" \
        --log-file "/log/file/for/the/transaction"

### snapdir-gcs-store test

Run integration tests for the GCS store.

Requires valid Google Cloud credentials in your system.

You can set the credentials file by setting the environment variable:

- SNAPDIR_GCS_STORE_CREDENTIALS_FILE

Usage:

    snapdir-gcs-store-test --store="${STORE}"

Example:

    SNAPDIR_GCS_STORE_CREDENTIALS_FILE=/path/to/credentials.json \
    snapdir-gcs-store-test --store="gs://my-bucket/my-prefix"