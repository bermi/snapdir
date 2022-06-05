# snapdir-sqlite3-catalog

Logs manifest and push events to a sqlite3 database and allows
basic querying of the database.

## Background:

This is Reference implementation of snapdir catalog using
a local sqlite3 database. The methods in this file are
called by the snapdir script.

## Usage

    snapdir-sqlite3-catalog [OPTIONS] [SUBCOMMAND]

### Options

    --event=name           Event name that triggered a log entry.
    --debug                Enable debug output.
    --help, -h             Prints help message.
    --id=ID                Manifest ID to use.
    --location=DIR|STORE   Location for catalog queries.
    --verbose              Enable verbose output.
    --version, -v          Prints version.

### Commands

    ancestors --id=                 Get a list of ancestor snapdir IDs their location.
    help [COMMAND]                  Prints help information.
    locations                       Lists directories and stores where snapshots
                                    have been taken or published.
    log --id= --event= --location=  Saves an event. Calls save under the hood.
    revisions --location=           Get a list of snapdir IDs created on a
                                    location (store or abs path).
    save --id= --location=          Saves an entry and sets it's ancestor.
    test                            Runs unit tests.
    version                         Prints the version.

### Environment variables

    SNAPDIR_SQLITE3_BIN               Path to sqlite3 binary with json support.
    SNAPDIR_SQLITE3_CATALOG_DB_PATH   Path where the database will be created.
                                      Defaults to ~/.snapdir/catalog-production.sqlite3.db.
### Examples

    # Saves a log entry to the database for newly generated manifest.
    snapdir-sqlite3-catalog log --event "manifest" --id "${SNAP_MANIFEST_ID}" --location "/some/dir"

    # Saves a log entry to the database for newly pushed manifest.
    snapdir-sqlite3-catalog log --event "push" --id "${SNAP_MANIFEST_ID}" --location "s3://some-bucket/"

    # Lists all locations and stores where snapshots have been taken or published.
    snapdir-sqlite3-catalog locations

    # shows all the ancestors of a given snapdir ID.
    snapdir-sqlite3-catalog ancestors --id="${SNAPDIR_ID}"

    # shows all ancestors for a given snapdir ID in a given location.
    snapdir-sqlite3-catalog ancestors --id="${SNAPDIR_ID}" --location="s3://some-bucket/"

    # Gets a list of revisions stored on a store
    snapdir-sqlite3-catalog revisions --location="s3://my-bucket/some/path"

    # Gets a list of revisions stored on a local directory
    snapdir-sqlite3-catalog revisions --location="/home/user/some/path"

## API Reference

### snapdir-sqlite3-catalog log

Receives a log message from a a snapdir event.

This is the only write interface for the catalog.
Called after manifest generation and store pushing.

Usage:

    snapdir-sqlite3-catalog log \
        --event="$EVENT_NAME" \
        --location="${LOCATION}" \
        --id="${ID}"

### snapdir-sqlite3-catalog save

Saves an entry on the snapdir_history table.

This is not called directly by snapdir but is called
the snapdir_sqlite3_catalog_log function on a new subshell.

Usage:

    snapdir-sqlite3-catalog save \
        --location="${LOCATION}" \
        --id="${ID}"

### snapdir-sqlite3-catalog locations

Lists locations tracked by the catalog. These include local directories and stores.

Usage:

    snapdir-sqlite3-catalog locations

Returns: JSON lines of the form:

    {
        "created_at": "YYYY-MM-DD HH:MM:SS.SSS",
        "id": "${SNAPDIR_ID}",
        "location": "${ABSOLUTE_DIR_NAME_OR_STORE_URI}"
    }

Example:

    snapdir-sqlite3-catalog locations

### snapdir-sqlite3-catalog ancestors

Get a list of ancestor snapdir IDs and the location where they where created.

Usage:

    snapdir-sqlite3-catalog ancestors \
        --id="${SNAPDIR_ID}" \
        [--location="${ABSOLUTE_DIR_NAME_OR_STORE_URI}"]

Returns: JSON lines of the form:

    {
        "created_at": "YYYY-MM-DD HH:MM:SS.SSS",
        "id": "${PARENT_SNAPDIR_ID}",
        "location": "${ABSOLUTE_DIR_NAME_OR_STORE_URI}"
    }

Examples:

    snapdir-sqlite3-catalog ancestors --id="${SNAPDIR_ID}"
    snapdir-sqlite3-catalog ancestors --id="${SNAPDIR_ID}" --location="s3://some-bucket/"

### snapdir-sqlite3-catalog revisions

Get a list of snapdir IDs created on a specific location.

Usage:

    snapdir-sqlite3-catalog revisions \
        --location="${ABSOLUTE_DIR_NAME_OR_STORE_URI}"

Returns: JSON lines of the form:

    {
        "created_at": "YYYY-MM-DD HH:MM:SS.SSS",
        "id": "${SNAPDIR_ID}",
        "previous_id": "${PREVIOUS_SNAPDIR_ID}",
        "location": "${ABSOLUTE_DIR_NAME_OR_STORE_URI}"
    }

Example2:

    # Gets a list of revisions stored on a store
    snapdir-sqlite3-catalog revisions --location="s3://my-bucket/some/path"

    # Gets a list of revisions stored on a local directory
    snapdir-sqlite3-catalog revisions --location="/home/user/some/path"

### snapdir-sqlite3-catalog test

Runs tests for the snapdir-sqlite3-catalog

Usage:

    snapdir-sqlite3-catalog test
