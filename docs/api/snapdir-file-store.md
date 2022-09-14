# snapdir-file-store

Reference implementation of snapdir store using the filesystem.

## API Reference

### snapdir-file-store get-push-command

Gets the command for pushing the contents of the staging directory to the store.
The staging directory is a temporary directory that is used to hold
files that are not yet available on the store.


    snapdir-file-store get-push-command \
        --id "${snapdir_id}" \
        --staging-dir "${staging_directory}" \
        --store "${store}"

### snapdir-file-store get-manifest-command

Gets the command for echoing the contents of a manifest given its ID.
This method does not save the manifest on the cache (that's done by
snapdir), it just prints the contents of the manifest so that
the files contained on it can be transferred by calling the
commands from the snapdir_file_store_get_fetch_files_command method.

Example:

			snapdir-file-store get-manifest-command --id "${id}" --store "${store}"

### snapdir-file-store get-fetch-files-command

Generates the command or commands required to download
to the cache the files defined on a manifest.
In order to mantain consistency when reading manifests
manifests will not exist on the local cache until
all the objects have been fetched. Therefore this
function will read the manifest contents from stdin.

Example:

	cat some_manifest_file | \
      snapdir-file-store get-fetch-files-command \
      --id "${id}" \
      --store "file:///long/term/storage/" \
      --cache-dir "/tmp/snapdir-cache"

### snapdir-file-store ensure-no-errors

This method is called once all the .objects in the manifest have been
transferred to or from the store.
Errors will be sent to stderr and the process will exit with
a non-zero status.

Example:

    snapdir-file-store verify-transactions \
        --checksum "aa91e498f401ea9e6ddbaa1138a0dbeb030fab8defc1252d80c77ebefafbc70d" \
        --log-file "/log/file/for/the/transaction"

### snapdir-file-store commit-manifest

This method is called once all the .objects in the manifest have been
transferred to the store. The log file will be inspected for errors
and the manifest will be committed if there are no errors.
We call this as the last step of the push operation.

Example:

    commit-manifest \
        --checksum "aa91e498f401ea9e6ddbaa1138a0dbeb030fab8defc1252d80c77ebefafbc70d" \
        --source-path "/path/to/local/manifest_file" \
        --target-path "/path/to/long/term/manifest_file" \
        --log-file "/log/file/for/the/transaction"

### snapdir-file-store fetch-object

Fetches a single object from the store.

### snapdir-file-store commit-object

Commits a single object to the store.

### snapdir-file-store test

Runs the tests for the file store

Usage:

    snapdir-file-store test
