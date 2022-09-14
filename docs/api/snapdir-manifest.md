# snapdir-manifest

Description:

    Generate authenticated directory structure manifests using Merkle trees.

## Usage

    snapdir-manifest [OPTIONS] [COMMAND] [ARGUMENTS]

### Options

    --absolute               Uses absolute paths.
    --cache                  Enables caching.
    --cache-dir=DIR          Sets cache directory.
    --cache-id=ID            Ensures the cache has a specific
                             ID before trusting it.
    --checksum-bin=NAME      Sets the name of the checksum
                             binary (default: b3sum).
    --debug                  Prints debug messages.
    --exclude=PATTERN        Excludes paths matching PATTERN.
                             set to "%system%" to default to
                             $SNAPDIR_SYSTEM_EXCLUDE_DIRS
    -h, --help               Prints help message.
    --no-follow              Prevents following symlinks.
    --verbose                Prints verbose messages.
    -v, --version            Prints version.

### Commands

    cache-id           Gets the id for the cache.
    flush-cache        Flushes the cache.
    defaults           Prints default options and env variables.
    generate <PATH>    Generates a manifest for a directory (default
                       when no other sub-command is provided).
    help [COMMAND]     Prints help information.
    test               Tests the snapdir-manifest module.
    version            Prints the version.

### Arguments

    <PATH>    The path to the directory to generate a manifest.

### Environment variables

    SNAPDIR_MANIFEST_BIN_PATH      Test-only path to a snapdir-manifest binary.
    SNAPDIR_MANIFEST_CONTEXT       Context string for deriving key in keyed mode.
                                   This only works with b3sum.
    SNAPDIR_SYSTEM_EXCLUDE_DIRS    Directories to exclude on --exclude="%system%".

### Examples

    # generates a manifest for the current directory
    snapdir-manifest ./

    # excludes files and directories matching the pattern
    snapdir-manifest --exclude=".git|.DS_Store" ./some-dir/

    # uses cache and shows details
    snapdir-manifest --cache --verbose ./

    # gets the integrity checksum for the cache directory
    snapdir-manifest cache-id

    # uses the cache integrity checksum to verify the cache
    trusted_cache_id=$(snapdir-manifest cache-id)
    snapdir-manifest --cache --cache-id "$trusted_cache_id" ./

    # generates a manifest for a whole system, excluding system files
    snapdir-manifest --absolute --exclude="%system%" --no-follow /

## Manifest specification

The manifest is a plain text file UTF-8 encoded list of files and
directories sorted in their paths. It contains the following columns
separated by spaces:

    PATH_TYPE PERMISSIONS CHECKSUM SIZE PATH

Where:

- *`PATH_TYPE`*: "*F*" for files, "*D*" for directories. Symbolic
  links include the type of the target.
- *`PERMISSIONS`*: The permissions of the file or directory in octal.
- *`CHECKSUM`*: The checksum of the file or directory, according to
  the `--checksum-binary=<name>` option. By default, `b3sum`. For
  directories, we sort the checksum of the objects in the directory
  and then concatenate them without spaces or newlines between them to
  compute the checksum. Check the manual example in the [understanding
  manifests guide](../understanding-manifests.md).
  Duplicated checksums are removed before the checksum is computed.
- *`SIZE`*: The size of the file or directory contents in bytes. It
  does not include the size for the directory metadata as reported by
  `stat`; it is only the sum of all the elements in the directory.
- *`PATH`*: The file or directory path. When using `--absolute` will
  resolve to the absolute path.

### Source code and issues

https://github.com/bermi/snapdir-manifest

## API Reference

### snapdir-manifest

Default command. Alias for: snapdir-manifest generate

Generates a manifest for a directory.

Usage:

    snapdir-manifest \
        [--(absolute|cache|no-follow|verbose)] \
        [--cache-dir="${CACHE_DIR}"] \
        [--cache-id="${ID}"] \
        [--checksum-bin=b3sum|md5sum|sha256sum] \
        [--exclude="${EXCLUDE_PATTERN}"] \
        "${DIR}"

Examples:

    # generates a manifest for a directory
    snapdir-manifest  "${DIR}"

    # generates a manifest for the root directory using
    # absolute paths. This assumes --exclude=system
    snapdir-manifest --absolute /

    # generates a manifest using the cache and validating
    # a previously known cache id
    snapdir-manifest --cache \
        --cache-id "${CACHE_ID}" \
        --cache-dir "${CACHE_DIR}" "${DIR}"

    # excludes files matching the pattern
    snapdir-manifest --exclude ".ignore" "${DIR}"

    # excludes files matching the pattern while
    # keeping the default common and system patterns
    snapdir-manifest --exclude ".ignore|%common%|%system%" "${DIR}"

    # use sha256sum as the checksum algorithm
    snapdir-manifest --checksum-bin sha256sum "${DIR}"

    # use a custom secret for b3sum context
    SNAPDIR_MANIFEST_CONTEXT="${SECRET}" snapdir-manifest "${DIR}"

### snapdir-manifest flush-cache

Empties the cache directory.

Usage:

    snapdir-manifest flush-cache [--cache-dir "${CACHE_DIR}"]

### snapdir-manifest cache-id

Computes the hash for the cache at its current state.

You can store the generated ID on a trusted system and use it to
check if the cache has changed or has been tampered with.

Usage:

    snapdir-manifest [--cache-dir "${CACHE_DIR}"]

### snapdir-manifest defaults

Shows the default enviroment variables and argument values.

### snapdir-manifest test

Runs the tests for the snapdir-manifest command.

Requires the helper script [snapdir-test](./snapdir-test) to be on the same directory.

Usage:

    snapdir-manifest test
