# Snapdir

Create, audit and distribute authenticated directory snapshots.

[Snapdir] enables creating, sharing and verifying snapshots of directories and their contents using human readable manifests.

In its current incarnation, pre v1.0 [Snapdir] has been implemented as independent and tested bash scripts.

| program                                      | description                                                                                                       | docs  ![docs status]                  | status               |
|----------------------------------------------|-------------------------------------------------------------------------------------------------------------------|------------------------------------------------------------|----------------------|
| [`snapdir`](./snapdir)                       | Snapshotting, verification and sharing of directories     with pluggable storage backends.                        | [install](./docs/install.md), [manual](./docs/api/snapdir.md) | ![unit tests status] |
| [`snapdir-manifest`](./snapdir-manifest)     | Standalone tool for creating directory snapshot manifests that can be versioned controlled and audited by humans. | [manual](./docs/api/snapdir-manifest.md)                      | ![unit tests status] |
| [`snapdir-file-store`](./snapdir-file-store) | Storage backend using the filesystem.                                                                             | [manual](./docs/api/snapdir-file-store.md)                    | ![unit tests status] |
| [`snapdir-s3-store`](./snapdir-s3-store)     | Storage backend using Amazon S3.                                                                                  | [manual](./docs/api/snapdir-s3-store.md)                      | ![s3 status]         |
| [`snapdir-b2-store`](./snapdir-b2-store)     | Storage backend using Backblaze B2.                                                                               | [manual](./docs/api/snapdir-b2-store.md)                      | ![b2 status]         |
| [bermi/snapdir] docker image                 | 8MB Docker image containing snapdir and all its dependencies.                                                     | [install](./docs/api/snapdir-docker.md)                       | ![docker status]     |


The main [goal](./README.md#design-goals) of [Snapdir] pre v1.0 is to define [an auditable manifest format](./docs/understanding-manifests.md) easy to support and implement in all programming languages.

## What is Snapdir?

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
-   Use external object backends like Amazon S3 for persistence
    and sharing, and structure simple to expose via HTTP.
-   Allow files to be replicated and updated concurrently without
    coordination.
-   Optional deduplication of files by using links to cached files.
-   Allow balancing performance and correctness by offering off-process
    integrity checks and deduplication.
-   Allow verifying snapshots using cryptographic hashes and standard
    UNIX tools.
-   Use of deterministic ID's to replicate and share snapshots.
-   Performant and efficient post v1.0.0 release using a compiled language.

### Non-goals

While this project remains a prototype built for experimentation, we 
expect some features to be missing from the `bash` version.

-   Multiple Operating Systems support. Only Linux and macOS (with bash>5) are supported.
-   Compression or encryption of files at rest. While this might be
    desirable, it will complicate the `snapdir` manifests spec.
-   Real-time or streaming files are not efficient targets for
    [snapdir], as it assumes files are immutable and the format needs to
    be human-readable.
-   ACL's and authentication. Remote object backends are well suited for
    this.

## Pluggable Stores

[Snapdir] delegates to stores the task of persisting fetching files on
long-term storage.

When calling snapdir `fetch`, `pull` or `push` methods you must supply a
valid `--store` option which determines the source or origin of the data.
The `--store` argument is formatted as a URI, where the store name is taken
from the protocol part of the URI. For example, `file://some/path` is a
valid `--store` as long as there's a `snapdir-file-store` binary somewhere
in your `PATH`.

Check the [authoring stores documentation](./docs/authoring-stores.md) for more details.

## Installation

Snapdir requires [snapdir-manifest] and \[b3sum\] for creating
manifests.

After installing the dependencies, download the [snapdir] script and
save it somewhere in your `PATH`.

```bash
wget -p https://raw.githubusercontent.com/bermi/snapdir/main/snapdir -O snapdir
chmod +x snapdir
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

The following `alias` will expose your current directory as `/target`

```bash
alias snapdir='docker run -it --rm \
    -v "$(realpath .):/target" \
    --workdir /target \
    -v "${HOME}/.cache/snapdir:/root/.cache/snapdir" \
    bermi/snapdir'
```



### Alternatives

There are many other tools that might be better suited for your particular use case. For example: [ostree](https://ostreedev.github.io/ostree/introduction/), [mtree](https://www.freebsd.org/cgi/man.cgi?mtree\(8\)), [Git LFS](https://git-lfs.github.com/), [DVC](https://dvc.org/), [Syncthing](https://syncthing.net/), [BitTorrent](https://en.wikipedia.org/wiki/BitTorrent), [DAT](https://dat-ecosystem.org/), [git](https://git-scm.com/), [HDF5](https://en.wikipedia.org/wiki/Hierarchical_Data_Format), [tar](https://www.gnu.org/software/tar/), [Btrfs](https://en.wikipedia.org/wiki/Btrfs), [ZFS](https://en.wikipedia.org/wiki/ZFS), [IPFS](https://ipfs.io/), [Perkeep](https://perkeep.org/), [SeaweedFS](https://github.com/chrislusf/seaweedfs),
[upspin](https://upspin.io/) [Keybase Filesystem](https://book.keybase.io/docs/crypto/kbfs) and [Sigstore](https://www.sigstore.dev/).

We use `Snapdir` in conjunction with some of the tools mentioned above.
None of them met the simplicity, ergonomics and auditability goals we had in mind when defining `Snapdir`.


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
