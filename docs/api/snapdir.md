# snapdir

Create, audit and distribute authenticated directory snapshots.

## Usage

    snapdir [OPTIONS] [SUBCOMMAND] [ARGUMENTS]

### Options

    --cache-dir=DIR        Directory where the object cache is stored.
    --context=DIR|STORE    Context for logger queries.
    --debug                Enable debug output.
    --dryrun               Run without making any changes.
    --exclude=PATTERN      Excludes paths matching PATTERN.
                           set to "%system%" to default to
                           $SNAPDIR_SYSTEM_EXCLUDE_DIRS
    --force                Force an action to run.
    --help, -h             Prints help message.
    --id=ID                Manifest ID to use.
    --keep                 Keeps the staging directory.
    --linked               Use symlinks instead of copies.
    --logger=NAME          Logger to use. Defaults to $SNAPDIR_LOGGER.
    --paths=PATTERN        Only includes paths matching PATTERN
                           when checking out manifests.
    --purge                Purges objects with invalid checksums.
    --store=URI            Store URI protocol://location/path.
    --verbose              Enable verbose output.
    --version, -v          Prints version.

### Commands

    ancestors --id=                Get a list of ancestor snapdir IDs their context.
                                   Requires --logger.
    checkout --id= [--linked] DIR  Checkout a snapshot to a directory.
    contexts                       Lists directories and stores where snapshots
                                   have been taken or published. Requires --logger.
    defaults                       Prints default settings and arguments.
    fetch --id= --store=           Fetch a snapshot from a store.
    flush-cache                    Flushes the local cache.
    help [COMMAND]                 Prints help information.
    id [PATH]                      Prints the manifest ID of a directory
                                   or manifest provided via stdin.
    manifest PATH                  Prints the manifest of a directory.
    pull --id= --store= PATH       Fetches a snapshot from a store and checks
                                   it out the given path.
    push --store= [--id=] [PATH]   Pushes a snapshot to a store given its path or
                                   a staged manifest ID.
    revisions --context=           Get a list of snapdir IDs created on a
                                   context (store or abs path). Requires --logger.
    stage DIR                      Saves into the local cache a snapshot of
                                   a directory.
    test                           Runs unit tests for snapdir.
    verify --id= [--purge]         Verifies the integrity of a staged snapshot.
    verify-cache [--purge]         Verifies the integrity of the local cache.
    version                        Prints the version.

### Arguments

    <DIR>    Directory path for snapshot operations.
    <PATH>   Path to an object in a manifest to cherry-pick.

### Environment variables

    SNAPDIR_CACHE_DIR              Where are the object cache files stored?
                                   Defaults to $HOME/.snapdir/cache and will be
                                   overridden by --cache-dir.
    SNAPDIR_MANIFEST_CONTEXT       Context string for deriving key in keyed mode.
    SNAPDIR_SYSTEM_EXCLUDE_DIRS    Directories to exclude on --exclude="%system%".
    SNAPDIR_LOGGER                 Default logger to use when no --logger is
                                   provided. Defaults to none.

### Examples

    # generates a manifest for the current directory
    snapdir manifest ./

    # generates an id for the manifest of the current directory
    snapdir id ./

    # excludes files and directories matching the pattern
    snapdir --exclude="/(.git|.DS_Store)($|/)" manifest ./

    # stages a snapshot of the current directory
    snapdir stage ./

    # verifies a snapshot given its id, to purge invalid objects add --purge
    snapdir verify --verbose --id=d640dce8e26f39d4dae336a7da83478385ce52a844c1d9b91f204ef83c558dd2

    # checks out a copy of the snapshot on a given directory
    snapdir checkout --id=d640dce8e26f39d4dae336a7da83478385ce52a844c1d9b91f204ef83c558dd2 ./two

    # use --linked if you prefer to create a hard link and use a COW filesystem
    snapdir checkout --link --id=d640dce8e26f39d4dae336a7da83478385ce52a844c1d9b91f204ef83c558dd2 ./two

## API Reference

### snapdir manifest

Prints on the stdout a manifest for a directory or staged manifest ID.

Usage:

    snapdir manifest \
        [--(id="${MANIFEST_ID}")] \
        [--(exclude="${EXCLUDE_PATTERN}")] \
        [--cache-dir="${CACHE_DIR}"] \
        [--stage] \
        "${DIR}"

Returns: [A snapdir manifest](docs/understanding-manifests.md) as
         a UTF-8 encoded string.

Examples:

    # generates a manifest for a directory
    snapdir manifest "${DIR}"

    # generates a manifest for a directory and stages it
    # creating a copy of the objects in the local cache
    snapdir manifest --stage "${DIR}"

    # excludes files matching the pattern
    snapdir manifest --exclude ".ignore" "${DIR}"

			# exclude commonly ignored files, such as .git and .DS_Store
			snapdir manifest --exclude "%common%" "${DIR}"

    # excludes files matching the pattern while
    # keeping the default common pattern
    snapdir manifest --exclude ".ignore|%common%" "${DIR}"

    # use a custom secret for b3sum context
    SNAPDIR_MANIFEST_CONTEXT="${SECRET}" snapdir manifest "${DIR}"

    # reads the directory from stdin
     echo "${DIR}" | snapdir manifest

    # shows the manifest for an staged manifest ID
    snapdir manifest --id "${MANIFEST_ID}"

    # shows the manifest given a staged manifest ID trhough stdin
    echo "${MANIFEST_ID}" | snapdir manifest

### snapdir id

Generates a snapshot id for a given directory and writes it to stdout.

Usage:

    snapdir id \
        [--stage] \
        [--cache-dir="${CACHE_DIR}"] \
        "${DIR}"

Returns: Snapshot ID (a BLAKE3 hash for the manifest contents)

Examples:

    # generates a snapshot id for a directory
    snapdir id "${DIR}"

    # generates a snapshot id for a directory and stages
    # the contents and the manifest id in the local cache
    snapdir manifest --stage "${DIR}"

    # generates a snapshot id for a manifest provided as stdin
    echo "${DIR}" | snapdir manifest | snapdir id

### snapdir push

Pushes a directory snapshot to a store.

This method will stage the snapshot and then push it to the store.

The STORE is a URI of the form:

    [store:]//[bucket|host]/[path]

For example: s3://my-bucket/my-snapshots or file:///tmp/my-snapshots

Usage:

    snapdir push \
        --store="${STORE}" \
        [--id="${ID}"] or ["${DIR}"] \
        [--cache-dir="${CACHE_DIR}"] \
        [--(debug|dryrun|verbose)]

Returns: snapshot id

Examples:

    # pushes a snapshot of a directory to a store
    snapdir push --store="${STORE}" "${DIR}"

    # pushes a staged snapshot id to a store
    snapdir push --store="${STORE}" --id="${ID}"

    # pushes the stdin provided snapshot id to a store
    snapdir id "${DIR}" | snapdir push --store="${STORE}"

	 # show the snapdir-$adapter-store command that would be executed
    # for a dry run
    snapdir push --store="${STORE}" --verbose --dryrun "${DIR}"

### snapdir fetch

Retrieves a snapshot from a store and saves it in the local cache.

Usage:

    snapdir fetch \
        --store="${STORE}" \
        --id="${ID}" \
        [--(dryrun|verbose)] \
        [--cache-dir="${CACHE_DIR}"]

Returns: No output unless in --verbose mode. Exit code 1 in case of error.

Examples:

    # fetches a snapshot from a store and saves it in the local cache
    snapdir fetch --store="${STORE}" --id="${ID}" --verbose

    # dry run
    snapdir fetch --store="${STORE}" --id="${ID}" --dryrun

### snapdir pull

Fetches a snapshot from a store and checks it out on a given directory.

Usage:

    snapdir pull \
        --store="${STORE}" \
        --id="${ID}" \
        [--(dryrun|verbose|force|linked)] \
        [--cache-dir="${CACHE_DIR}"] \
        [--paths="${COMMA_SEPARATED_PATH_PATTERNS}"] \
        ["${DIR}"]

Returns: No output unless in --verbose mode. Exit code 1 in case of error.

Examples:

    # fetches a snapshot and checks it out
    snapdir pull --store="${STORE}" --id="${ID}" --verbose "${DIR}"

    # forces a checkout even if the directory contains modified files
    snapdir pull --store="${STORE}" --id="${ID}" --force "${DIR}"

    # dry run
    snapdir pull --store="${STORE}" --id="${ID}" --dryrun "${DIR}"

### snapdir checkout

Checks out a snapshot from the cache on a given directory.

When the linked option is used, the files are hardlinked instead of copied.
This only works if the files are on the same filesystem and it's only
recommended for filesystems with copy-on-write (COW) support.

Usage:

    snapdir checkout \
        --store="${STORE}" \
        --id="${ID}" \
        [--(dryrun|verbose|force|linked)] \
        [--cache-dir="${CACHE_DIR}"] \
        [--paths="${COMMA_SEPARATED_PATH_PATTERNS}"] \
        ["${DIR}"]

Returns: No output unless in --verbose mode. Exit code 1 in case of error.

Examples:

    # checks out a snapshot
    snapdir checkout --store="${STORE}" --id="${ID}" --verbose "${DIR}"

    # forces a checkout even if the directory contains modified files
    snapdir checkout --store="${STORE}" --id="${ID}" --force "${DIR}"

    # dry run
    snapdir checkout --store="${STORE}" --id="${ID}" --dryrun "${DIR}"

    # hardlinks files instead of copying them
    snapdir checkout --store="${STORE}" --id="${ID}" --linked "${DIR}"

    # only include paths matching the given prefixes
    snapdir checkout --store="${STORE}" --id="${ID}" --paths="configs,i18n" "${DIR}"

### snapdir stage

Stages the files in a given directory into the local cache.

Usage:

    snapdir stage ["${DIR}"] \
        [--cache-dir="${CACHE_DIR}"]
        [--(keep|verbose)]

Returns: The manifest id or the staging directory when --keep is provided.

Examples:

    # Stages the current directory so that it can be
    # pushed to the store or checked out elsewhere locally.
    snapdir stage

    # stages a given directory and keeps the staging directory
    snapdir stage "${DIR}" --keep # this prints the staging directory

    # stages a to a custom cache directory
    snapdir stage "${DIR}" --cache-dir="${CACHE_DIR}" --verbose

### snapdir verify

Verifies the integrity of a snapshot.

Usage:
    snapdir verify \
        --id="${ID}" \
        [--cache-dir="${CACHE_DIR}"]
        [--(purge|verbose)]

Returns: Exits with 0 if the snapshot is valid, 1 otherwise.

Examples:

    # verify a snapshot that has been cached locally
    snapdir verify --id="${ID}" --verbose

    # verify and purge invalid objects from the cache
    snapdir verify --id="${ID}" --purge

### snapdir contexts

Lists contexts tracked by the looger. These include local directories and stores.

Usage:

    snapdir contexts \
        [--logger="sqlite3"]

Returns: JSON lines of the form:

    {
        "created_at": "YYYY-MM-DD HH:MM:SS.SSS",
        "id": "${SNAPDIR_ID}",
        "context": "${ABSOLUTE_DIR_NAME_OR_STORE_URI}"
    }

Example:

    # Use the default logger defined by SNAPDIR_LOGGER
    snapdir contexts

### snapdir ancestors

Get a list of ancestor snapdir IDs and the context where they where created.

Usage:

    snapdir ancestors \
        --id="${SNAPDIR_ID}" \
        [--context="${ABSOLUTE_DIR_NAME_OR_STORE_URI}"] \
        [--logger="sqlite3"]

Returns: JSON lines of the form:

    {
        "created_at": "YYYY-MM-DD HH:MM:SS.SSS",
        "id": "${PARENT_SNAPDIR_ID}",
        "context": "${ABSOLUTE_DIR_NAME_OR_STORE_URI}"
    }

Example:

    # Use the default logger defined by SNAPDIR_LOGGER
    snapdir ancestors --id="${SNAPDIR_ID}"

### snapdir revisions

Get a list of snapdir IDs created on a specific context.

Usage:

    snapdir revisions \
        --context="${ABSOLUTE_DIR_NAME_OR_STORE_URI}" \
        [--logger="sqlite3"]

Returns: JSON lines of the form:

    {
        "created_at": "YYYY-MM-DD HH:MM:SS.SSS",
        "id": "${SNAPDIR_ID}",
        "previous_id": "${PREVIOUS_SNAPDIR_ID}",
        "context": "${ABSOLUTE_DIR_NAME_OR_STORE_URI}"
    }

Example:

    # Gets a list of revisions stored on a store
    snapdir revisions --context="s3://my-bucket/some/path"

    # Gets a list of revisions stored on a local directory
    snapdir revisions --context="/home/user/some/path"

### snapdir verify-cache

Verifies the integrity of all the objects in the cache.

Usage:

    snapdir verify-cache \
        [--cache-dir="${CACHE_DIR}"]
        [--(purge|verbose)]

Returns: Exits with 0 if all the objects are valid, 1 otherwise.

Example:

    # verify all the objects in the cache and purges invalid objects
    snapdir verify-cache --verbose --purge

### snapdir flush-cache

Empties the local cache.

Usage:

    snapdir verify-cache \
        [--cache-dir="${CACHE_DIR}"]

### snapdir defaults

Shows the default environment variables and options.

Usage:

    snapdir defaults

### snapdir test

Runs the tests for the snapdir command.

Requires the helper script [snapdir-test](./snapdir-test) to be on the same directory.

Usage:

    snapdir test
