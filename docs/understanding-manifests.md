# Manifest Guide

This guide will help you to understand what the command
[snapdir-manifest] does under the hood and how third-party tools could
use the snapdir manifest format.

## Primer into the manifest format

Let's look at a practical example.

On a terminal, we will first create an example directory with some test
files:

``` bash
mkdir -p example
umask 077 example
mkdir -p example/a/
echo a1 >example/a/a1
echo a2 >example/a/a2
echo "base" > example/base
```

The following command will create a manifest for the directory structure
we've just created:

``` bash
snapdir-manifest ./example
```

Which outputs:

    D 700 4257cc46336b9d0ae70a3104ae0382ac6a75da0ee49ffe69b423997e872276a7 11 ./
    D 700 40bdff878af8e7ffbc40f1d4b5a72c892a0773df2d47cd164c2dc2e684299dfa 6 ./a/
    F 600 92719755f8d6c804d44192bb5835654d27003fc8fdbb36a633b9063c7f9396a4 3 ./a/a1
    F 600 ff3e86a123552d66c31eb3308916d76bf9d918b1f635aa39d00d3a3428bda536 3 ./a/a2
    F 600 b9af5f26c46534d25add40a12c3f0b1ae926e39a2e669162664295040943f54a 5 ./base

This is the main product of the `snapdir-manifest` command, which will
disect in the next section.

### Manifest format

The manifest is a text file that contains a sorted list of files and
directories. Each line in the manifest has the following columns
separated a space:

    1.PATH_TYPE    2.PERMISSIONS    3.CHECKSUM    4.SIZE    5.PATH

Lets go over each one of the columns in the output and explain what they
mean.

1.  *`PATH_TYPE`*: "*F*" for files, "*D*" for directories. Symbolic
    links include the type of the target.
2.  *`PERMISSIONS`*: The permissions of the file or directory in octal.
3.  *`CHECKSUM`*: The checksum of the file or directory, according to
    the `--checksum-binary=<name>` option. By default, `b3sum`. For
    directories, we sort the checksum of the objects in the directory
    and then concatenate them without spaces or newlines between them to
    compute the checksum. Check the manual example below.
4.  *`SIZE`*: The size of the file or directory contents in bytes. It
    does not include the size for the directory metadata as reported by
    `stat`; it is only the sum of all the elements in the directory.
5.  *`PATH`*: The file or directory path. When using `--absolute` will
    resolve to the absolute path.

### Manifest comments

Lines starting with `#` are ignored and should be removed before
computing the manifest checksum. This can be used to store details
about how the manifest was created, or where the storage is located.

## Manually creating a manifest

We'll now recreate the checksums manually using `b3sum`. First, generate
the checksums of the files:

    b3sum ./example/a/a1
    # outputs: 92719755f8d6c804d44192bb5835654d27003fc8fdbb36a633b9063c7f9396a4
    b3sum ./example/a/a2
    # outputs: ff3e86a123552d66c31eb3308916d76bf9d918b1f635aa39d00d3a3428bda536
    b3sum ./example/base
    # outputs: b9af5f26c46534d25add40a12c3f0b1ae926e39a2e669162664295040943f54a

The outputs are the same as the ones we previously got from the
manifest.

Now let's compute the checksum for the directory `./a/` by combining the
checksums of `./a/a1` and `./a/a2`:

``` bash
b3sum ./example/a/* --no-names | sort |  tr -d '\n' | b3sum --no-names
# outputs: 40bdff878af8e7ffbc40f1d4b5a72c892a0773df2d47cd164c2dc2e684299dfa
```

The previous command sorts the output of `b3sum` and removes the
newlines before computing the checksum of the concatenated file
checksums.

Finally, we can verify the checksum of the directory `./example/` by
combining the checksum of the file `./example/base` and the checksum of
the directory `./example/a/`:

``` bash
echo -n \
  $(b3sum ./example/a/* --no-names | sort |  tr -d '\n' | b3sum --no-names)\
  $(b3sum --no-names ./example/base) | tr -d ' ' | b3sum --no-names
# outputs: 4257cc46336b9d0ae70a3104ae0382ac6a75da0ee49ffe69b423997e872276a7
```

As we can the final output matches the one we got on the
snapdir-manifest the manifest.

## Further reading

Now that you understand how the manifest format works, you can use
`snapdir` to create, verify and distribute the contents of the
manifests.

  [snapdir-manifest]: https://github.com/bermi/snapdir/tree/main/snapdir-manifest-README.md
