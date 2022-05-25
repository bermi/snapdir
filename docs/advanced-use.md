# Snapdir advanced use

## --exclude patterns

    # using the %common% pattern to exclude common hidden files
    snapdir --exclude="%common%" manifest ./

    # snapdir of the root filesystem ignoring system and common files
    snapdir --exclude="%system%|%common%" manifest /
