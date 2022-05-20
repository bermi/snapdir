# Contributing to snapdir

The `Snapdir` project is a community effort. We welcome contributions
from everyone.

## v1 goals

The goals for v1 are:

-   To define a sound developer experience.
-   Agreeing on a manifest format.
-   Create an extensive test suite.
-   Educate the community on how to use Snapdir.

To reach this goal we've choosen `bash` to pipe togheter existing unix
tools.

## v2 goals

Our long-term goal is to create a portable `snapdir` executable that can
be used in any environment with zero configuration or dependencies.
We'll choose a safe systems programming language to do this.

The design of the project allows swapping the individual `snapdir-*`
bash scripts for implementations in other languages gradually.

## Development

If you use VSCode, the `.devcontainer/devcontainer.json` will create a
[Docker environment] with the required dependencies for testing and
linting the code.

## Linting and formatting

The maintenability of the project is a priority, so we've chosen to lint
the code using shellcheck and keep a consistent format via shfmt.

The following script can be saved as on `.git/hooks/pre-commit` as a git
hook to replicate the linting and formatting that takes on the CI
pipeline.

``` bash
#!/bin/bash

set -eEuo pipefail

# Run for every snapdir file that's been changed
for script in $(git diff --name-only HEAD | grep "^snapdir" | grep -v ".md"); do
  echo "Running $script"
  # lint
  shellcheck ./"$script"
  git diff --exit-code -- ./"$script" || {
    echo "'./$script' has changes that have not been staged. Please stage or stash them." >&2
    exit 1
  }

  # format
  shfmt -w -s ./"$script"
  git diff --exit-code -- ./"$script" || {
    echo "'./$script' has been reformatted by shfmt. Please review the changes and stage them." >&2
    exit 1
  }

  # test
  ./"$script" test
done

docker build -t snapdir .
```

  [Docker environment]: .devcontainer/Dockerfile.ubuntu
