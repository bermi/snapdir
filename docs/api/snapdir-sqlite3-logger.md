# snapdir-sqlite3-logger

Logs manifest and push events to a sqlite3 database and allows
basic querying of the database.

## Background:

 This is Reference implementation of snapdir logger using
 a local sqlite3 database.

## API Reference

### snapdir-sqlite3-logger log

Receives a log message from a a snapdir event.

This is the only write interface for the logger and will
so far it's only called after manifest generation and
store pushing.

### snapdir-sqlite3-logger save

Saves an entry on the snapdir_history table.

This is not called directly by snapdir but is called
the snapdir_sqlite3_logger_log function on a new subshell.

### snapdir-sqlite3-logger contexts

Lists contexts tracked by the looger. These include local directories and stores.

Usage:

    snapdir-sqlite3-logger contexts

Returns: JSON lines of the form:

    {
        "created_at": "YYYY-MM-DD HH:MM:SS.SSS",
        "id": "${SNAPDIR_ID}",
        "context": "${ABSOLUTE_DIR_NAME_OR_STORE_URI}"
    }

Example:

    snapdir-sqlite3-logger contexts

### snapdir-sqlite3-logger ancestors

Get a list of ancestor snapdir IDs and the context where they where created.

Usage:

    snapdir-sqlite3-logger ancestors \
        --id="${SNAPDIR_ID}" \
        [--context="${ABSOLUTE_DIR_NAME_OR_STORE_URI}"]

Returns: JSON lines of the form:

    {
        "created_at": "YYYY-MM-DD HH:MM:SS.SSS",
        "id": "${PARENT_SNAPDIR_ID}",
        "context": "${ABSOLUTE_DIR_NAME_OR_STORE_URI}"
    }

Examples:

    snapdir-sqlite3-logger ancestors --id="${SNAPDIR_ID}"
    snapdir-sqlite3-logger ancestors --id="${SNAPDIR_ID}" --context="s3://some-bucket/"

### snapdir-sqlite3-logger revisions

Get a list of snapdir IDs created on a specific context.

Usage:

    snapdir-sqlite3-logger revisions \
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
    snapdir-sqlite3-logger revisions --context="s3://my-bucket/some/path"

    # Gets a list of revisions stored on a local directory
    snapdir-sqlite3-logger revisions --context="/home/user/some/path"

### snapdir-sqlite3-logger test

note: using subshell – '(' instead of '{' – to avoid leaking helper functions
