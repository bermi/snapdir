# Snapdir

Audit and distribute authenticated directory snapshots.

![unit tests status] ![s3 status] ![b2 status] ![docker status] ![docs status]

[Snapdir] enables creating, sharing and verifying snapshots of
directories and their contents.

Built as a set of independent and well tested bash scripts, [Snapdir]
provides the following functionality:

-   `snapdir-manifest`: Standalone tool for creating and verifying
    directory snapshot manifests that can be versioned controlled. A
    building block for \[content-addressable storage applications\] and
    [conflict-free replicated data type]
    ([CRDT][conflict-free replicated data type]) strategies.
-   `snapdir`: Snapshotting, verification and sharing of directories
    with pluggable storage backends.
-   `snapdir-file-store`: Reference implementation of a storage backend
    using the filesystem.

Check the [documentation for more information].

## Installation

Snapdir requires [snapdir-manifest] and \[b3sum\] for creating
manifests.

After installing the dependencies, download the [snapdir] script and
save it somewhere in your `PATH`.

```bash
wget -p https://github.com/bermi/snapdir/releases/download/v0.1.1/snapdir -O snapdir
chmod +x snapdir
echo "7350e268ecbfc0d03c37621480ba501862f8f9904eb28349136f5eea9251fb4f  snapdir" | b3sum -c
mv snapdir-manifest /usr/local/bin/
```

## Try without installing

You can try [snapdir] using the Docker image [bermi/snapdir]

```bash
target_dir=./ # specify a target directory
# using -v to mount the target directory on the docker container
docker run -it --rm \
    -v "$(realpath $target_dir):/target" \
    -v "${HOME}/.cache/snapdir:/root/.cache/snapdir" \
    bermi/snapdir /target
```

Convenience wrapper for the dockerized version of [snapdir] that will
expose your current working directory as `/target`.

```bash
alias snapdir='docker run -it --rm \
    -v "$(realpath .):/target" \
    --workdir /target \
    -v "${HOME}/.cache/snapdir:/root/.cache/snapdir" \
    bermi/snapdir'
```


# snapdir

Take snapshots of your data and restore it to previous known states.

Snapdir is a userspace cli program with the following features:

- Generates manifests and unique identifiers of the
  contents of directories and files.
- Saves and restores data from pluggable storage
  backends such as Amazon S3 and Google Cloud Storage.
- Verifies the integrity of the data using cryptographic
  hashes.
- UNIX-style composability.
- Content addressable local object cache.

Snapdir is a building block for applications that need one or more
of the following characteristics:

- Storing data on untrusted environments.
- Content replicated data types (CRDTs).
- File-system based data replication.
- Data integrity verification.
- File deduplication.
- Multicloud file sharing.


## Usage

    snapdir [OPTIONS] [SUBCOMMAND] [ARGUMENTS]

### Options

    --cache-dir=DIR        Directory where the object cache is stored.
    --debug                Enable debug output.
    --dryrun               Run without making any changes.
    --force                Force an action to run.
    --help, -h             Prints help message.
    --id=ID                Manifest ID to use.
    --keep                 Keeps the staging directory.
    --linked               Use symlinks instead of copies.
    --path=PATH            Partial path for checkout operations.
    --purge                Purges objects with invalid checksums.
    --store=URI            Store URI protocol://location/path.
    --verbose              Enable verbose output.
    --version, -v          Prints version.

### Commands


    checkout --id= [--linked] DIR  Checkout a snapshot to a directory.
    defaults                       Prints default settings and arguments.
    fetch --id= --store=           Fetch a snapshot from a store.
    flush-cache                    Flushes the local cache.
    help                           Prints help information.
    id [PATH]                      Prints the manifest ID of a directory
                                   or manifest provided via stdin.  
    manifest PATH                  Prints the manifest of a directory.
    pull --id= --store= PATH       Fetches a snapshot from a store and checks
                                   it out the given path.
    push --store= [--id=] [PATH]   Pushes a snapshot to a store given its path or
                                   a staged manifest ID.
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

    SNAPDIR_MANIFEST_CONTEXT    Context string for deriving key in keyed mode.
                                This only works with b3sum.
    SNAPDIR_MANIFEST_EXCLUDE    Default grep -v rule for --exclude="system".

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

## Motivation

This tool was created as a prototype to explore an optimal workflow for
consuming and generating files in ephemeral environments. At
[BermiLabs], we used it to replicate parquet files in our analytics
pipelines and our distributed ETL workflows.

We decided to open source it could be used by others to implement
[CRDT][conflict-free replicated data type] strategies on eventually
consistent read-heavy applications.

### Design goals

-   Manifest format and specification should be simple to understand by
    humans and simple to implement.
-   Manifest format should be auditable and suitable for tracking under
    version control.
-   Simple and intuitive CLI interface for working with files and
    directories with UNIX-style composability and no configuration
    required.
-   Use external object backends like s3 or Backblaze b2 for persistence
    and sharing, and structure simple to expose via HTTP.
-   Allow files to be replicated and updated concurrently without
    coordination.
-   Optimized for fast initialization on read-only environments.
-   Optional deduplication of files by using links to cached files.
-   Allow balancing performance and correctness by offering off-process
    integrity checks and deduplication.
-   Allow verifying snapshots using cryptographic hashes and standard
    UNIX tools.
-   Use of deterministic \`id's to replicate and share snapshots.
-   Performant and efficient v1.0.0 release using a compiled language.

### Non-goals

While this project remains a prototype built for experimentation, we
expect some features to be missing from the `bash` version.

-   Multiple Operating Systems support. Only Linux and macOS (with bash
    \>5) are supported.
-   Compression or encryption of files at rest. While this might be
    desirable, it will complicate the `snapdir` manifests spec.
-   Real-time or streaming files are not efficient targets for
    [snapdir], as it assumes files are immutable and the format needs to
    be human-readable.
-   ACL's and authentication. Remote object backends are well suited for
    this.

### Alternatives

-   [Git LFS] Why not
    https://towardsdatascience.com/why-git-and-git-lfs-is-not-enough-to-solve-the-machine-learning-reproducibility-crisis-f733b49e96e8
-   DVC (md5sum)
-   HDF5
-   Tar
-   ZFS
-   [ostree], requires `ostree` to verify snapshots.
-   manifest generations: mtree
    (`mtree -p <dir> -c -k mode,size,time,link,sha256`)
-   IPFS
-   Perkeep, SeaweedFS
-   upspin sharing via email
-   Keybase Filesystem (signing manifests)
-   Sigstore

Why not use git? We want to optimize download speed and deduplication
performance to avoid cold starts. We don't care about history, just the
latest state, and we want to be able to leverage CDNs and HTTP caches
for sharing snapshots.

## **Integrity**: Snapshots are generated from a directory, and their contents are verified against a manifest.

## Usage

    snapdir \
        <checkout|fetch|flush-cache|help|id|manifest|pull|push|stage|verify|verify-cache|test|integration-test> \
        (--force|keep|linked|purge|verbose) \
        (--id|path|store=<value>) (<base_dir>)

### Quick reference

    checkout --id=<ID> <DIR>
    checkout --link --id=<ID> <DIR>
    fetch --id=<ID> --store=<STORE>
    id <DIR>
    manifest --exclude="%common%" <DIR>
    manifest --exclude="%system%|%common%" <DIR>
    manifest --exclude="/(.git|.DS_Store)($|/)" <DIR>
    manifest <DIR>
    pull --id=<ID> --store=<STORE>
    pull --id=<ID> --store=<STORE> <DIR>
    push --id=<ID> --store=<STORE>
    push --store=<STORE>
    push --store=<STORE> <DIR>
    stage <DIR>
    stage <DIR> --keep
    test
    verify --id=<ID>
    verify --purge --id=<ID>


## Optional arguments

The following are the optional arguments and their defaults:

-   --store=b2://\${SNAPDIR_B2_STORE_BUCKET_NAME}
-   --cwd=\$(pwd)
-   --path
-   --debug=false
-   --script_path=path to this script
-   --verbose=false
-   --force=false

## Stores

[Snapdir] delegates to stores the task of persisting fetching files on
long-term storage.

When calling snapdir `fetch`, `pull` or `push` methods you must supply a
valid `--store` option which determines the source or origin of the data.
The `--store` argument is formatted as a URI, where the store name is taken
from the protocol part of the URI. For example, `file://some/path` is a
valid `--store` argument that will use the `snapdir-file-store`.

Stores must be installed and available in the `PATH` of the calling
process. They will emmit commands that the `snapdir` CLI will execute or
display when running in `--dryrun` mode.

Check the [authoring stores documentation](./docs/authoring-stores.md) for more information.


## License

LICENSE: MIT Copyright (c) 2022 Bermi Ferrer

  [unit tests status]: https://github.com/bermi/snapdir/actions/workflows/unit_tests.yml/badge.svg
  [b2 status]: https://github.com/bermi/snapdir/actions/workflows/b2-store.yml/badge.svg
  [s3 status]: https://github.com/bermi/snapdir/actions/workflows/s3-store.yml/badge.svg
  [docs status]: https://github.com/bermi/snapdir/actions/workflows/docs.yml/badge.svg
  [docker status]: https://github.com/bermi/snapdir/actions/workflows/build.yml/badge.svg
  [Snapdir]: https://github.com/bermi/snapdir
  [conflict-free replicated data type]: https://en.wikipedia.org/wiki/Conflict-free_replicated_data_type
  [documentation for more information]: https://github.com/bermi/snapdir/tree/main/docs/
  [snapdir-manifest]: https://github.com/bermi/snapdir/tree/main/snapdir-manifest-README.md
  [bermi/snapdir]: https://hub.docker.com/r/bermi/snapdir/tags
  [BermiLabs]: https://bermilabs.com
  [Git LFS]: https://git-lfs.github.com/
  [ostree]: https://ostreedev.github.io/ostree/introduction/
