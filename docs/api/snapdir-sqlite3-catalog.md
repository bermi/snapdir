# snapdir-sqlite3-catalog

Logs manifest and push events to a sqlite3 database and allows
basic querying of the database.

## Background:

 This is Reference implementation of snapdir catalog using
 a local sqlite3 database.

## API Reference

### snapdir-sqlite3-catalog log

Receives a log message from a a snapdir event.

This is the only write interface for the catalog and will
so far it's only called after manifest generation and
store pushing.

### snapdir-sqlite3-catalog save

Saves an entry on the snapdir_history table.

This is not called directly by snapdir but is called
the snapdir_sqlite3_catalog_log function on a new subshell.

### snapdir-sqlite3-catalog contexts

Lists contexts tracked by the looger. These include local directories and stores.

Usage:

    snapdir-sqlite3-catalog contexts

Returns: JSON lines of the form:

    {
        "created_at": "YYYY-MM-DD HH:MM:SS.SSS",
        "id": "${SNAPDIR_ID}",
        "context": "${ABSOLUTE_DIR_NAME_OR_STORE_URI}"
    }

Example:

    snapdir-sqlite3-catalog contexts

### snapdir-sqlite3-catalog ancestors

Get a list of ancestor snapdir IDs and the context where they where created.

Usage:

    snapdir-sqlite3-catalog ancestors \
        --id="${SNAPDIR_ID}" \
        [--context="${ABSOLUTE_DIR_NAME_OR_STORE_URI}"]

Returns: JSON lines of the form:

    {
        "created_at": "YYYY-MM-DD HH:MM:SS.SSS",
        "id": "${PARENT_SNAPDIR_ID}",
        "context": "${ABSOLUTE_DIR_NAME_OR_STORE_URI}"
    }

Examples:

    snapdir-sqlite3-catalog ancestors --id="${SNAPDIR_ID}"
    snapdir-sqlite3-catalog ancestors --id="${SNAPDIR_ID}" --context="s3://some-bucket/"

### snapdir-sqlite3-catalog revisions

Get a list of snapdir IDs created on a specific context.

Usage:

    snapdir-sqlite3-catalog revisions \
        --context="${ABSOLUTE_DIR_NAME_OR_STORE_URI}"

Returns: JSON lines of the form:

    {
        "created_at": "YYYY-MM-DD HH:MM:SS.SSS",
        "id": "${SNAPDIR_ID}",
        "previous_id": "${PREVIOUS_SNAPDIR_ID}",
        "context": "${ABSOLUTE_DIR_NAME_OR_STORE_URI}"
    }

Example:

    # Gets a list of revisions stored on a store
    snapdir-sqlite3-catalog revisions --context="s3://my-bucket/some/path"

    # Gets a list of revisions stored on a local directory
    snapdir-sqlite3-catalog revisions --context="/home/user/some/path"

### snapdir-sqlite3-catalog test

note: using subshell – '(' instead of '{' – to avoid leaking helper functions
