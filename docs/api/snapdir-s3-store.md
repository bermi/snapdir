# snapdir-s3-store

## Description:

    Snapdir store backed by [Amazon S3 cli tool](https://awscli.amazonaws.com/v2/documentation/api/latest/reference/s3/index.html).

## API Reference

### snapdir-s3-store get-push-command

Gets the command for pushing the contents of the staging directory
Amazon S3.
The staging directory is a temporary directory that is used sync
the contents of a specific manifest to the s3 bucket.
We rely on 'aws s3' and 'aws s3api' to do the actual push and integrity
check.

    snapdir-s3-store get-push-command \
        --staging-dir "${staging_directory}" \
        --store "${store}"

### snapdir-s3-store get-manifest-command

Gets the command for echoing the contents of a manifest given its ID.
This method does not save the manifest on the cache (that's done by
snapdir), it just prints the contents of the manifest.

All

Example:

			snapdir-s3-store get-manifest-command --id "${id}" --store "${store}"

### snapdir-s3-store get-fetch-files-command

Generates the commands required to download from 
S3 to the local cache the files defined on a manifest.
Manifests will not exist on the local cache until
all the objects have been fetched. 
This function reads the manifest contents from stdin.

Usage:

	cat some_manifest_file | \
      snapdir-s3-store get-fetch-files-command \
      --id="${ID}" \
      --store="s3://bucket-name/long/term/storage/" \
      [--cache-dir="${CACHE_DIR}"]

### snapdir-s3-store get-manifest

Pipes a manifest given its ID to stdout.

Usage:

    snapdir-s3-store get-manifest \
        --id="${ID}" \
        --store="${STORE}" \
        [--retries=5]

### snapdir-s3-store fetch

Performs the actual fetching of files from the remote store.

Usage:

    snapdir-s3-store fetch \
        --store "${STORE}" \
        --checksum="${ID}" \
        --source-path="${SOURCE_FILE_PATH}" \
        --target-path="${REMOTE_FILE_PATH}" \
				  --log-file="${LOG_FILE_PATH}"

### snapdir-s3-store ensure-no-errors

This method is called once all the .objects in the manifest have been
transferred to or from the store.
Errors will be sent to stderr and the process will exit with
a non-zero status.

Usage:

    snapdir-s3-store verify-transactions \
        --checksum "aa91e498f401ea9e6ddbaa1138a0dbeb030fab8defc1252d80c77ebefafbc70d" \
        --log-file "/log/file/for/the/transaction"

### snapdir-s3-store test

note: using subshell – '(' instead of '{' – to avoid leaking helper functions
