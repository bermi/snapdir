# Snapdir store authoring guide

[Snapdir] delegates to stores the task of persisting fetching files on
long-term storage.

When calling snapdir `fetch`, `pull` or `push` methods you must supply a
valid `--store` option which determines which store to use. The
`--store` argument is formatted as a URI, and the store name is taken
from the protocol part of the URI. For example, `file://some/path` is a
valid `--store` argument that will use the `snapdir-file-store`.

stores must be installed and available in the `PATH` of the calling
process. They will emmit commands that the `snapdir` CLI will execute or
display when running in `--dryrun` mode.

The following methods must be implemented by the stores.

### get-manifest-command

Emmit a manifest to stdout given a manifest id.

    snapdir-${store_NAME}-store get-manifest-command --id "${snapdir_id}" --store "${store}"

### get-fetch-files-command

Generates the command or commands required to download to the cache the
files defined on the manifest provided via stdin that are not already
available locally.

    cat generate-manifest-somehow | snapdir-${store_NAME}-store get-fetch-files-command --store "${store}" --cache-dir "${cache_dir}"

### get-push-command

Gets the command for pushing the contents of the staging directory to
the store. The staging directory is a temporary directory that is used
to hold files that are not yet available on the store.

    snapdir-${store_NAME}-store get-push-command --staging-dir "${staging_directory}" --store "${store}"

A testing `snapdir-file-store` is provided as an example implementation.

