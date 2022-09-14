# Snapdir

Authenticated Directory Snapshots.

## What is snapdir?

Snapdir is a tool for creating and restoring snapshots of directories.

The main feature are:

- Generating manifests and unique identifiers of the
  contents of directories and files.
- Saving and restores data from pluggable storage
  backends such as Amazon S3 and Backblaze B2.
- Verifying the integrity of the data using cryptographic
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

Snapdir was created as a prototype to explore an optimal workflow for
consuming and generating files in ephemeral environments. At
[BermiLabs], we used it to replicate parquet files in our analytics
pipelines and our distributed ETL workflows.

We decided to open source it could be used by others to implement
[CRDT][conflict-free replicated data type] strategies on eventually
consistent read-heavy applications.


## Usage

### Prerequisites

Snapdir requires [BLAKE3] for hashing and HMAC signing and optionally [sqlite]
to query local snapshots.

To verify your dependencies are on your `$PATH` run:

```bash
command -v b3sum
command -v sqlite3
```

To install the dependencies on debian flavored distributions you can run:

```bash
apt-get install -y wget sqlite3
wget -q "https://github.com/BLAKE3-team/BLAKE3/releases/download/1.3.1/b3sum_linux_x64_bin" -O /usr/local/bin/b3sum
chmod +x /usr/local/bin/b3sum
```

### Installation

[Snapdir] has been implemented as independent and tested bash scripts.

| program        
                              | description                                                                                                       | docs  ![docs status]                  | status               |
|----------------------------------------------|-------------------------------------------------------------------------------------------------------------------|------------------------------------------------------------|----------------------|
| [`snapdir`](./snapdir)                       | Snapshotting, verification and sharing of directories     with pluggable storage backends.                        | [install](./docs/install.md), [manual](./docs/api/snapdir.md) | ![unit tests status] |
| [`snapdir-manifest`](./snapdir-manifest)     | Standalone tool for creating directory snapshot manifests that can be versioned controlled and audited by humans. | [README](./docs/snapdir-manifest.md) [manual](./docs/api/snapdir-manifest.md) | ![unit tests status] |
| [`snapdir-file-store`](./snapdir-file-store) | Storage backend using the filesystem.                                                                             | [manual](./docs/api/snapdir-file-store.md)                    | ![unit tests status] |
| [`snapdir-s3-store`](./snapdir-s3-store)     | Storage backend using Amazon S3.                                                                                  | [manual](./docs/api/snapdir-s3-store.md)                      | ![s3 status]         |
| [`snapdir-b2-store`](./snapdir-b2-store)     | Storage backend using Backblaze B2.                                                                               | [README](./docs/snapdir-b2-store.md) [manual](./docs/api/snapdir-b2-store.md) | ![b2 status]         |
| [`snapdir-sqlite3-catalog`](./snapdir-sqlite3-catalog) | Basic catalog of local and remote manifests.                                                          | [manual](./docs/api/snapdir-sqlite3-catalog.md) | ![catalog status]         |
| [bermi/snapdir] docker image                 | 8MB Docker image containing snapdir and all its dependencies.                                                     | [install](./docs/api/snapdir-docker.md)                       | ![docker status]     |


At a minimum, snapdir requires the `snapdir` and `snapdir-manifest` scripts to
be on your `$PATH`.

The following command installs the following scripts: `snapdir`,
`snapdir-manifest`, `snapdir-s3-store`, `snapdir-test` and `snapdir-sqlite3-catalog`
in `/usr/local/bin/`

```bash
for script in snapdir snapdir-manifest snapdir-s3-store snapdir-sqlite3-catalog snapdir-test; do
    wget -p "https://raw.githubusercontent.com/bermi/snapdir/main/${script}" -O "$script"
    chmod +x "$script"
    mv "$script" /usr/local/bin/
done
```

### Via Docker

You can try [snapdir] using the Docker image [bermi/snapdir]

```bash
target_dir=./ # specify a target directory
# using -v to mount the target directory on the docker container
docker run -it --rm \
    -v "$(realpath $target_dir):/target" \
    -v "${HOME}/.cache/snapdir:/root/.cache/snapdir" \
    bermi/snapdir manifest /target
```


## Contributing

Snapdir is licensed under the MIT License and contributions are welcome! Please check the [contributing guidelines](https://github.com/bermi/snapdir/blob/main/CONTRIBUTING.md) and visit the [github repo](https://github.com/bermi/snapdir) for more information.

To checkout the code and run tests:

```bash
git clone https://github.com/bermi/snapdir.git
cd snapdir
./snapdir-test
```

There project includes a VSCode devcontainer configuration that you can use to develop snapdir in a containerized environment.

## Alternatives

There are many other tools that might be better suited for your particular use case. For example: [ostree](https://ostreedev.github.io/ostree/introduction/), [mtree](https://www.freebsd.org/cgi/man.cgi?mtree\(8\)), [Git LFS](https://git-lfs.github.com/), [DVC](https://dvc.org/), [Syncthing](https://syncthing.net/), [BitTorrent](https://en.wikipedia.org/wiki/BitTorrent), [DAT](https://dat-ecosystem.org/), [git](https://git-scm.com/), [HDF5](https://en.wikipedia.org/wiki/Hierarchical_Data_Format), [tar](https://www.gnu.org/software/tar/), [Btrfs](https://en.wikipedia.org/wiki/Btrfs), [ZFS](https://en.wikipedia.org/wiki/ZFS), [IPFS](https://ipfs.io/), [Perkeep](https://perkeep.org/), [SeaweedFS](https://github.com/chrislusf/seaweedfs),
[upspin](https://upspin.io/) [Keybase Filesystem](https://book.keybase.io/docs/crypto/kbfs) and [Sigstore](https://www.sigstore.dev/).

We use `Snapdir` in conjunction with some of the tools mentioned above.
None of them met the simplicity, ergonomics and auditability goals we had in mind when defining `Snapdir`.


  [unit tests status]: https://github.com/bermi/snapdir/actions/workflows/unit_tests.yml/badge.svg
  [b2 status]: https://github.com/bermi/snapdir/actions/workflows/b2-store.yml/badge.svg
  [s3 status]: https://github.com/bermi/snapdir/actions/workflows/s3-store.yml/badge.svg
  [catalog status]: https://github.com/bermi/snapdir/actions/workflows/sqlite3-catalog.yml/badge.svg
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
  [BLAKE3]: https://github.com/BLAKE3-team/BLAKE3
