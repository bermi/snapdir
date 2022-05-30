# snapdir-manifest

Generate authenticated directory structure manifests using [Merkle
trees].

Available as a [single-file bash script], [snapdir-manifest] allows
capturing the structure, integrity checksums and permissions of
directories and their contents.

Use [snapdir-manifest] as a building block for [content-addressable
storage applications], [hierarchical storage management (HSM)] and
[conflict-free replicated data type (CRDT)] strategies.

[snapdir-manifest] is part of the [Snapdir] project but can be used as a
standalone tool.

## Installation

Download the [snapdir-manifest] script and save it somewhere in your
`PATH`.

``` bash
wget -p https://raw.githubusercontent.com/bermi/snapdir/main/snapdir-manifest -O snapdir-manifest
chmod +x snapdir-manifest
mv snapdir-manifest /usr/local/bin/
```

By default, [snapdir-manifest] uses [BLAKE3] for hashing and HMAC
signing.

To use other hash functions, add `--checksum-bin=sha256sum` to the
commands on this README.

## Try without installing

Try [snapdir-manifest] using the Docker image [bermi/snapdir]

``` bash
target_dir=./ # specify a target directory
# using -v to mount the target directory on the docker container
docker run -it --rm \
    -v "$(realpath $target_dir):/target" \
    --entrypoint /bin/snapdir-manifest \
    bermi/snapdir /target
```

## Usage

    snapdir-manifest [OPTIONS] [SUBCOMMAND] [ARGUMENTS]

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
                             set to "system" to default to
                             $SNAPDIR_MANIFEST_EXCLUDE
    -h, --help               Prints help message.
    --no-follow              Prevents following symlinks.
    --verbose                Prints verbose messages.
    -v, --version            Prints version.

### Commands

    cache-id           Gets the id for the cache.
    flush-cache        Flushes the cache.
    defaults           Prints default options and env variables.
    help               Prints help information.
    manifest <PATH>    Generates a manifest for a directory (default
                       when no other sub-command is provided).
    test               Tests the snapdir-manifest module.
    version            Prints the version.

### Arguments

    <PATH>    The path to the directory to generate a manifest.

### Environment variables

    SNAPDIR_MANIFEST_BIN_PATH   Test-only path to a snapdir-manifest binary.
    SNAPDIR_MANIFEST_CONTEXT    Context string for deriving key in keyed mode.
                                This only works with b3sum.
    SNAPDIR_MANIFEST_EXCLUDE    Default grep -v rule for --exclude="system".

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
    snapdir-manifest --absolute --exclude=system --no-follow /

## Manifest specification

The manifest is a plain text file UTF-8 encoded list of files and
directories sorted in their paths. It contains the following columns
separated by spaces:

    PATH_TYPE PERMISSIONS CHECKSUM SIZE PATH

Where:

-   *`PATH_TYPE`*: "*F*" for files, "*D*" for directories. Symbolic
    links include the type of the target.
-   *`PERMISSIONS`*: The permissions of the file or directory in octal.
-   *`CHECKSUM`*: The checksum of the file or directory, according to
    the `--checksum-binary=<name>` option. By default, `b3sum`. For
    directories, we sort the checksum of the objects in the directory
    and then concatenate them without spaces or newlines between them to
    compute the checksum. Check the manual example in the [understanding
    manifests guide].
-   *`SIZE`*: The size of the file or directory contents in bytes. It
    does not include the size for the directory metadata as reported by
    `stat`; it is only the sum of all the elements in the directory.
-   *`PATH`*: The file or directory path. When using `--absolute` will
    resolve to the absolute path.

The [Undestanding manifests guide][understanding manifests guide] goes
into more details with a practical example.

## Features and design goals

-   Manifest format and specification can be audited by humans using
    existing tools.
-   Plain text manifest format suitable for being tracked under version
    control.
-   Single file `bash` script with an embedded test suite. Alternative
    implementations should be able to verify their compatibility by
    re-using the tests included in snapdir-manifest.
-   Supports caching and cache integrity validation.
-   Multiple hash algorithms supported. Default is [BLAKE3].
-   Optional Keyed Hashes (HMAC).
-   4MB [Dockerized version][bermi/snapdir] available.

## Limitations

-   Only Linux and macOS are actively tested. Other UNIX environments
    with a modern `bash` version should work. The dockerized version
    `docker run --rm -v ${PWD}:/target bermi/snapdir /target` works in
    all environments, including Windows.
-   HMAC is only available when using the default `blake3sum`.
-   Recursive symlinks will only be followed once or fail, depending on
    the underlying `find` command.

## Background

[snapdir-manifest] was built as part of the [Snapdir
repo][snapdir-manifest].

As part of `Snapdir` we needed to generate a manifest and decided to
extract the manifest generation code as a standalone tool following UNIX
small and single-purpose composable programs philosophy.

We first created a `bash` version to share the manifest format with the
community and gather feedback. Once the interfaces are well established,
we hope to see versions of this tool implemented in more efficient
languages.

Since its current implementation is a bash script, it is likely to be
less performant than a single process compiled version on systems with a
large number of files.

Once the interface and APIs are finalized and stable, we will look into
improving the portability and the performance by re-writing it in Rust,
Go or TypeScript (Deno).

## Security concerns

### Cache attacks

When using the `--cache` option, a malicious user can tamper with the
files in the cache directory. The cache id can be captured with the
command `snapdir-manifest cache-id` and stored in an external trusted
system to mitigate this attack. By adding the `--cache-id=` option to
subsequent [snapdir-manifest] calls, the cache integrity will be
verified automatically against tampering when the process begins.

### Manifest forging

To prevent third parties from forging a manifest, the
`SNAPDIR_MANIFEST_CONTEXT` environment variable can be used.
`SNAPDIR_MANIFEST_CONTEXT` will be required to verify the manifest. A
long `SNAPDIR_MANIFEST_CONTEXT` will make the manifest less vulnerable
to brute force attacks.

### Preimage attacks

When using checksum algorithms with known collision attacks such as
`md5` or `sha1`, there is the possibility of a second preimage attack,
where an attacker creates documents that generate the same root hash. To
avoid this scenario, the checksum of the manifest can be stored on a
trusted location to ensure its integrity.

## Alternatives

Multiple alternatives such as [ostree], [Sigstore][] [cosign], `git`,
`dat` or Keybase Filesystem (signing manifests) have a broader scope
than this library.

The closest tool we found when building [snapdir-manifest] was BSD's
[mtree].

## Contributing

Contributions and Pull Requests are welcome. Please see the
[contributing guide] for more information.

## License

LICENSE: MIT Copyright (c) 2022 Bermi Ferrer

  [Merkle trees]: https://en.wikipedia.org/wiki/Merkle_tree
  [single-file bash script]: https://github.com/bermi/snapdir/blob/main/snapdir-manifest
  [snapdir-manifest]: https://github.com/bermi/snapdir
  [content-addressable storage applications]: https://en.wikipedia.org/wiki/Content-addressable_storage
  [hierarchical storage management (HSM)]: https://en.wikipedia.org/wiki/Hierarchical_storage_management
  [conflict-free replicated data type (CRDT)]: https://en.wikipedia.org/wiki/Conflict-free_replicated_data_type
  [Snapdir]: https://github.com/bermi/snapdir/
  [BLAKE3]: https://github.com/BLAKE3-team/BLAKE3
  [bermi/snapdir]: https://hub.docker.com/r/bermi/snapdir/tags
  [understanding manifests guide]: ./understanding-manifests.md
  [ostree]: https://ostreedev.github.io/ostree/introduction/
  [Sigstore]: https://www.sigstore.dev/
  [cosign]: https://github.com/sigstore/cosign
  [mtree]: https://www.freebsd.org/cgi/man.cgi?mtree(8)
  [contributing guide]: CONTRIBUTING.md
