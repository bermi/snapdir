# Snapdir Guide

This guide will show you how to use Snapdir through the command line.

The audience for this guide are developers who want to understand how
`snapdir` can be used as a building block for their own projects.

First, follow the [install guide] and make sure:

``` bash
snapdir --help
```

works.

Alternativelly you can use the [bermi/snapdir] docker image to follow
this guide from a shell with all the dependencies baked in by running:

``` bash
docker run -it --rm --entrypoint /bin/bash  bermi/snapdir
```

## Exploring the manifest

Lets create a new directory with some files on it:
    
    umask 077
    mkdir -p ~/snapdir-guide/example/
    cd ~/snapdir-guide/
    touch example/{foo,bar}.txt

You can see a manifest of the files in the directory by calling:

    snapdir manifest example
    # Outputs:
    # D 700 dba5865c0d91b17958e4d2cac98c338f85cbbda07b71a020ab16c391b5e7af4b 0 ./
    # F 600 af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262 0 ./bar.txt
    # F 600 af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262 0 ./foo.txt

You should see the same output as long as the directory and file
permissions are the same on your machine.

The columns on the previous output are:

-   D or F for a file or a directory.
-   The permissions of the file or directory.
-   The BLAKE3 message diggest (`b3sum`) of the path contents.
-   The size in bytes of the file or directory contents.
-   The path to the file or directory.

At this point, foo.txt and bar.txt are empty files, so their `b3sum`
matches.

`b3sum example/*` generates a similar output; this is not a
coincidence as we are using `b3sum` under the hood for checking the
integrity of the files.

The directory checksum is computed by concatenating the unique checksums
of the files without newlines or spaces.

We can compute it manually by running:

    b3sum --no-names example/* | sort -u | tr -d '\n' | b3sum  --no-names
    # Outputs: dba5865c0d91b17958e4d2cac98c338f85cbbda07b71a020ab16c391b5e7af4b

Directory checksums only use direct children files and directories
checksums and do not recurse into subdirectories.

Now that we have an understanding of the manifest format, let's create
an ID for the manifest itself by calling:

    snapdir manifest example | b3sum --no-names
    # Outputs: c678a299380893769bd7795628b96147229b410a9d5a5b7cae563bcae3c27857

The `id` we just generated is now a reference to the directory at its
current state.

Since writting the previous command is a little tedious, we can use the
`snapdir id` command instead:

    snapdir id example
    # Outputs: c678a299380893769bd7795628b96147229b410a9d5a5b7cae563bcae3c27857

After staging them locally, we can reference the snapshot `id` to create
copies of the directory contents.

## Caching snapshots

So far, we have not done anything with the `snapdir` that could not be
done with `b3sum` directly. To capture the state of the directory, we
can use the `stage` command.

### staging changes

The `stage` command saves to `${HOME}/.cache/snapdir` the objects and
manifests we want to keep track of. You can change the default location
by setting the `--cache-dir` option.

You won't use the `stage` command manually in your `snapdir` workflows,
but it's essential to understand how it works, so we'll go over it here.

Run the following command to stage the files locally, and save the
directory name to a variable:

    STAGED_DIR=$(snapdir stage example --keep | tee /dev/stderr)
    # Outputs: /tmp/snapdir_some_random_id

If we inspect the directory on the previous output, we can see a copy of
the manifest and the files (only one since they are deduplicated) we
want to keep track of:

    find ${STAGED_DIR} ! -type d
    # Outputs:
    # ${STAGED_DIR}/.objects/af1/349/b9f/5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262
    # ${STAGED_DIR}/.manifests/c67/8a2/993/80893769bd7795628b96147229b410a9d5a5b7cae563bcae3c27857

While the manifest content is copied verbatim and the files are linked
to `${HOME}/.cache/snapdir/.objects/` as we can see in the following
command:

    readlink -f ${STAGED_DIR}/.objects/af1/349/b9f/5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262
    # Outputs: ${HOME}/.cache/snapdir/.objects/af1/349/b9f/5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262

The first nine characters of the `b3sum` (*af1349b9f*) are used to
create a folder structure *af1/349/b9f/* that allows us to list
manifests and objects more efficiently on storage engines.

The cache directory is used globally for all snapshots. As we'll see
later, the `fetch` command also brings store snapshots into the cache
directory. Files must be placed on the local cache before being checked
out into directories or persisted on store storage engines.

Let's remove the staging directory since it is no longer for the rest of
the snapdir guide.

    rm -rf "${STAGED_DIR}"

We will now add some content to the files and stage a new snapshot.

    echo "foo" > example/foo.txt
    snapdir stage example
    # Outputs: 8af03a1bec09b1838d2c4f56c6940ed35ccdad1064243d2d775e8347ba82b9be

Since we didn't include the `--keep` flag, the output now shows the `id`
of the manifest and there's no `${STAGED_DIR}` directory.

The `id` the `stage` command generated is the same as the the one we get
by running the following command:

    snapdir id example
    # Outputs: 8af03a1bec09b1838d2c4f56c6940ed35ccdad1064243d2d775e8347ba82b9be

The manifest is now staged on the cache as
`${HOME}/.cache/snapdir/.manifests/8af/03a/1be/c09b1838d2c4f56c6940ed35ccdad1064243d2d775e8347ba82b9be`.
Let's inspect it:

    cat ${HOME}/.cache/snapdir/.manifests/8af/03a/1be/c09b1838d2c4f56c6940ed35ccdad1064243d2d775e8347ba82b9be
    # Outputs:
    # D 700 4a0732cfb45ebe9d8d572fc4c77b759384bed029911e35f8859430b889427d4d 4 ./
    # F 600 af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262 0 ./bar.txt
    # F 600 49dc870df1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92 4 ./foo.txt

The `id` we've got via `snapshot id` is the same we could have gotten by
running the `b3sum` command directly against the staged manifest:

    b3sum --no-names ${HOME}/.cache/snapdir/.manifests/8af/03a/1be/c09b1838d2c4f56c6940ed35ccdad1064243d2d775e8347ba82b9be
    # Outputs: 8af03a1bec09b1838d2c4f56c6940ed35ccdad1064243d2d775e8347ba82b9be

The benefit of having `id`s based on checksums is that they can be
verified against tampering and audited using `b3sum` without the need of
relying or trusting on `snapdir`.

## Checking out snapshots

Lets remove the `example` directory

    rm -rf example

and `checkout` the snapshot to restore the contents of `example` by using the previous `id`:

    snapdir checkout --id=8af03a1bec09b1838d2c4f56c6940ed35ccdad1064243d2d775e8347ba82b9be example
    cat example/foo.txt
    # Outputs: foo

We can still checkout the id of the original snapshot, which will bring
back the empty files.

    snapdir checkout --id=c678a299380893769bd7795628b96147229b410a9d5a5b7cae563bcae3c27857 example
    # Outputs: File ${HOME}/snapdir-guide/example/foo.txt already exists. To override file, use the --force flag.

as you can see, it has refused to override `foo.txt` unless `--force` is
provided. We don't need to do that to continue with this guide.

Now lets add some content for the bar.txt file and stage it.

    echo "bar" > example/bar.txt
    snapdir stage example
    # Outputs: df4b3a7b6c04e5b14ebb548a28ac0dea6c645f0ecfde85df2c0911ac10d2e8a9

### Linking objects

A way to save space is to checkout snapshots using the `--linked` flag,
when using the `stage` command which will create a hard link to the
objects. We'll not cover this mode in this guide since it's only useful
if your use case can ensure that the linked objects will not be modified
by using a [copy-on-write (COW) filesystem].

## Verifying snapshots

We can verify the integrity of a snapshot by calling:

    snapdir verify --verbose --id df4b3a7b6c04e5b14ebb548a28ac0dea6c645f0ecfde85df2c0911ac10d2e8a9
    # Outputs:
    # ${HOME}/.cache/snapdir/.manifests/df4/b3a/7b6/c04e5b14ebb548a28ac0dea6c645f0ecfde85df2c0911ac10d2e8a9: OK
    # ${HOME}/.cache/snapdir/.objects/b31/99d/36d/434044e6778b77d13f8dbaba32a73d9522c1ae8d0f73ef1ff14e71f: OK
    # ${HOME}/.cache/snapdir/.objects/49d/c87/0df/1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92: OK

This uses `b3sum --check` to verify the integrity of the snapshot stored
on the cache.

Lets tamper one of the objects in the cache and verify the integrity of
the snapshot again:

    echo "tampered" > ${HOME}/.cache/snapdir/.objects/b31/99d/36d/434044e6778b77d13f8dbaba32a73d9522c1ae8d0f73ef1ff14e71f
    snapdir verify --verbose --id df4b3a7b6c04e5b14ebb548a28ac0dea6c645f0ecfde85df2c0911ac10d2e8a9
    # Outputs:
    # ${HOME}/.cache/snapdir/.manifests/df4/b3a/7b6/c04e5b14ebb548a28ac0dea6c645f0ecfde85df2c0911ac10d2e8a9: OK
    # ${HOME}/.cache/snapdir/.objects/b31/99d/36d/434044e6778b77d13f8dbaba32a73d9522c1ae8d0f73ef1ff14e71f: FAILED
    # ${HOME}/.cache/snapdir/.objects/49d/c87/0df/1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92: OK

There are three ways to remove tampered objects from the cache.

1.  Using the `--purge` option when calling the verify command:
    `snapdir verify --purge --id df4b3a7b6c04e5b14ebb548a28ac0dea6c645f0ecfde85df2c0911ac10d2e8a9`
2.  Stage the example directory again:
    `snapdir stage example`
3.  Run a global cleanup command via: `snapdir verify-cache --purge`

We'll use the second option to remove the tampered object since it will
re-generate the object in the cache.

    snapdir stage example
    # Outputs:
    # ${HOME}/.cache/snapdir/.objects/b31/99d/36d/434044e6778b77d13f8dbaba32a73d9522c1ae8d0f73ef1ff14e71f ${HOME}/snapdir-guide/example/bar.txt differ: char 1, line 1
    # df4b3a7b6c04e5b14ebb548a28ac0dea6c645f0ecfde85df2c0911ac10d2e8a9

## Pushing snapshots

So far, we've learned how to keep a snapshot in sync with the files on
your local system. Let's now push the snapshot to a remote store
repository.

To push to a remote store a `snapdir-<STORE_NAME>-store` must exist on
your path.

The file store is suitable for storing snapshots on networked
filesystems, we'll be using it for this guide since it doesn't require
any external account.

If you want to use the Backblaze b2 adapter, the [snapdir-b2-store
documentation] guide will take over from this point.

To push the contents of `example` to the file store repository in
the `${HOME}/snapdir-guide/data` directory we can run:

    snapdir push --store "file://${HOME}/snapdir-guide/data" example
    # Outputs: df4b3a7b6c04e5b14ebb548a28ac0dea6c645f0ecfde85df2c0911ac10d2e8a9

If you run into issues, you can use the `--verbose` and `--debug`
options to get more information about the push.

Let's clear our local cache and the example directories:

    rm -rf ${HOME}/.cache/snapdir example

At this point the data is only available on the file store at `"file://${HOME}/snapdir-guide/data"`.    

Pulling from the remote with the following command will recreate the
local cache and the example directory:

    snapdir pull --id=df4b3a7b6c04e5b14ebb548a28ac0dea6c645f0ecfde85df2c0911ac10d2e8a9 --store "file://${HOME}/snapdir-guide/data" example
    cat example/{foo,bar}.txt
    # Outputs: foo
    # bar

If you only want to fetch the contents of the remote into the cache, you
can use the `fetch` method:

    rm -rf ${HOME}/.cache/snapdir
    snapdir fetch --id=df4b3a7b6c04e5b14ebb548a28ac0dea6c645f0ecfde85df2c0911ac10d2e8a9 --store "file://${HOME}/snapdir-guide/data"

## Conclussion

We have covered the basics of using snapdir using a local store. As a next
step you can try using one of the remote stores.

## Cleanup

To clean up the local cache and the example directory we created, you can run:

    rm -rf ${HOME}/.cache/snapdir ~/snapdir-guide/


  [install guide]: ./install.md
  [bermi/snapdir]: https://hub.docker.com/r/bermi/snapdir/tags
  [copy-on-write (COW) filesystem]: https://en.wikipedia.org/wiki/Copy-on-write
  [snapdir-b2-store documentation]: https://github.com/bermi/snapdir/tree/main/snapdir-b2-store-README.md
