# Snapdir

Audit and distribute authenticated directory snapshots.

![unit tests status] ![integration status] ![docker status] ![docs status]

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

    Syncs a store manifest generated by `b3sum` to a local directory.
    echo "$simple_manifest" | snapdir id "${_dir}"
    echo "$simple_manifest" | snapdir push --verbose --store="${store}" "${_dir}"
    snapdir checkout --force --id="${nested_manifest_id}" "${_dir}"
    snapdir checkout --force --id="${simple_manifest_id}" "${_dir}"
    snapdir checkout --verbose --id="${simple_manifest_id}" "${_dir}"
    snapdir checkout --verbose --id=${simple_manifest_id} "${_dir}"
    snapdir checkout --verbose --id=${simple_manifest_id} --path=foo "${_dir}"
    snapdir checkout --verbose --force --id=${simple_manifest_id} "${_dir}"
    snapdir fetch --verbose --store="${store}" --id="${simple_manifest_id}"
    snapdir fetch --verbose --store="${store}" --id=${simple_manifest_id}"
    snapdir flush-cache
    snapdir help
    snapdir id "${_dir}"
    snapdir manifest "${_dir}"
    snapdir pull --verbose --store="${store}" --id=${simple_manifest_id} ${_dir}"
    snapdir push --verbose --store="${store}" "${_dir}"
    snapdir stage "${_dir}"
    snapdir stage "${_dir}" >/dev/null
    snapdir verify --verbose --id=${simple_manifest_id}
    snapdir verify --verbose --purge --id=${simple_manifest_id}

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
  [integration status]: https://github.com/bermi/snapdir/actions/workflows/integration.yml/badge.svg
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
