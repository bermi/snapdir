# Installation

[snapdir] is implemented as a series of bash 5 scripts that need to be
saved somewhere in your `PATH`. It also relies on [b3sum] for
creating manifests.

If you're following this guide on a macOS
machine, you can install them by running the following command:

    brew install b3sum coreutils

On debian-like Linux, you can install them by running the following command:

    sudo apt-get install b3sum

Once you have the tools installed, the next step is to save the scripts in
your `PATH`.

The following scripts are required:

- [snapdir]: The main command.
- [snapdir-manifest]: Generates manifests for directories.

Depending on what store you choose, you'll need to install one of the following:

- [snapdir-file-store]: File system based store.
- [snapdir-b2-store]: Backblaze b2 based store.

Optionally, you can install [snapdir-test] to verify that snapdir works on your
system.

You can grab the commands from the [releases page].

## Docker image

You can try [snapdir] using the Docker image [bermi/snapdir].

Additionally you can extract the snapdir scripts to a `snapdir` directory
by callling:

```bash
docker run -it --rm \
  -v "$(pwd)/snapdir:/local" \
  --entrypoint /bin/bash \
  bermi/snapdir \
  -c "cp -R /bin/snapdir* /local/"

# copy the files on snapdir to /usr/local/bin
find ./snapdir -maxdepth 1 -perm /u=x,g=x,o=x  -type f -exec cp {} /usr/local/bin/ \;
```

## Verifying the installation

You can verify that the installation is working by running:

    snapdir-test


  [b3sum]: https://github.com/BLAKE3-team/BLAKE3/tree/master/b3sum
  [snapdir]: https://github.com/bermi/snapdir/blob/main/snapdir
  [snapdir-manifest]: https://github.com/bermi/snapdir/blob/main/snapdir-manifest
  [snapdir-test]: https://github.com/bermi/snapdir/blob/main/snapdir-test
  [snapdir-file-store]: https://github.com/bermi/snapdir/blob/main/snapdir-file-store
  [snapdir-b2-store]: https://github.com/bermi/snapdir/blob/main/snapdir-b2-store
  [releases page]: https://github.com/bermi/snapdir/releases/
  [bermi/snapdir]: https://hub.docker.com/r/bermi/snapdir/tags
