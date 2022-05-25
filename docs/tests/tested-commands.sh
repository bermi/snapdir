#!/usr/bin/env bash
# snapdir tested commands

snapdir checkout --force --id="${ID}" /tmp/snapdir_tests/files
snapdir checkout --verbose --force --id="${ID}" /tmp/snapdir_tests/files
snapdir checkout --verbose --id="${ID}" --linked /tmp/snapdir_tests/files
snapdir checkout --verbose --id="${ID}" --path=./a /tmp/snapdir_tests/files
snapdir checkout --verbose --id="${ID}" --path=a /tmp/snapdir_tests/files
snapdir checkout --verbose --id="${ID}" --path=a/ /tmp/snapdir_tests/files
snapdir checkout --verbose --id="${ID}" --path=foo /tmp/snapdir_tests/files
snapdir checkout --verbose --id="${ID}" /tmp/snapdir_tests/files
snapdir checkout --verbose --id="${ID}" snapdir-test-relative-dir
snapdir fetch --dryrun --store file:///tmp/snapdir-file-store_tests/store --id "${ID}"
snapdir fetch --store file:///tmp/snapdir-file-store_tests/store --id "${ID}" --verbose
snapdir fetch --store file:///tmp/snapdir-file-store_tests/store --id bogus --verbose
snapdir flush-cache
snapdir id /tmp/snapdir_tests/files
snapdir manifest --exclude=.ignored /tmp/snapdir_tests/files
snapdir manifest /tmp/snapdir_tests/files
snapdir pull --dryrun --verbose --store file:///tmp/snapdir-file-store_tests/store --id "${ID}" /tmp/snapdir-file-store_tests/files
snapdir pull --verbose --store file:///tmp/snapdir-file-store_tests/store --id "${ID}" /tmp/snapdir-file-store_tests/files
snapdir push --id "${ID}" --debug --verbose --store file:///tmp/snapdir-file-store_tests/store
snapdir push --id "${ID}" --verbose --store file:///tmp/snapdir-file-store_tests/store
snapdir push --verbose --dryrun --store file:///tmp/snapdir-file-store_tests/store /tmp/snapdir-file-store_tests/files
snapdir stage --keep /tmp/snapdir_tests/files
snapdir stage --verbose /tmp/snapdir_tests/files
snapdir stage /tmp/snapdir-file-store_tests/files
snapdir stage /tmp/snapdir_tests/files
snapdir test
snapdir verify --verbose --id="${ID}"
snapdir verify --verbose --purge --id="${ID}"
snapdir verify-cache
snapdir verify-cache --purge
snapdir version
snapdir-file-store commit-manifest --checksum "${ID}" --source-path /tmp/snapdir_"${ID}"/.manifests/"${ID_PATH}" --target-path /tmp/snapdir-file-store_tests/store/.manifests/"${ID_PATH}" --log-file /tmp/snapdir-"${ID}"
snapdir-file-store commit-object --checksum "${ID}" --source-path /tmp/snapdir_"${ID}"/.objects/"${ID_PATH}" --target-path /tmp/snapdir-file-store_tests/store/.objects/"${ID_PATH}" --log-file /tmp/snapdir-"${ID}"
snapdir-file-store ensure-no-errors --checksum "${ID}" --log-file /tmp/snapdir-"${ID}"
snapdir-file-store fetch-object --checksum "${ID}" --source-path /tmp/snapdir-file-store_tests/store/.objects/"${ID_PATH}" --target-path /tmp/snapdir-file-store_tests/.cache/snapdir-file-store/.objects/"${ID_PATH}" --log-file /tmp/snapdir-"${ID}"
snapdir-file-store get-fetch-files-command --id "${ID}" --store file:///tmp/snapdir-file-store_tests/store --cache-dir /tmp/snapdir-file-store_tests/.cache/snapdir-file-store
snapdir-file-store get-manifest-command --id "${ID}" --store file:///tmp/snapdir-file-store_tests/store
snapdir-file-store get-manifest-command --id bogus --store file:///tmp/snapdir-file-store_tests/store
snapdir-file-store get-push-command --id "${ID}" --staging-dir /tmp/snapdir_"${ID}"///tmp/snapdir-file-store_tests/store
snapdir-file-store test
snapdir-manifest --checksum-bin b3sum --absolute /tmp/snapdir-manifest_tests/files
snapdir-manifest --checksum-bin b3sum --cache --cache-id "${ID}" --cache-dir /tmp/snapdir-manifest_tests/cache /tmp/snapdir-manifest_tests/files
snapdir-manifest --checksum-bin b3sum --cache --cache-id bogus --cache-dir /tmp/snapdir-manifest_tests/cache /tmp/snapdir-manifest_tests/files
snapdir-manifest --checksum-bin b3sum --cache --verbose --cache-dir /tmp/snapdir-manifest_tests/cache /tmp/snapdir-manifest_tests/files
snapdir-manifest --checksum-bin b3sum --exclude=%common% /tmp/snapdir-manifest_tests/files
snapdir-manifest --checksum-bin b3sum --exclude=files/a /tmp/snapdir-manifest_tests/files
snapdir-manifest --checksum-bin b3sum ../files
snapdir-manifest --checksum-bin b3sum /tmp/snapdir-manifest_tests/files
snapdir-manifest --checksum-bin b3sum cache-id --cache-dir /tmp/snapdir-manifest_tests/cache
snapdir-manifest --exclude=.ignored /tmp/snapdir_tests/files
snapdir-manifest --no-follow --checksum-bin b3sum /tmp/snapdir-manifest_tests/files
snapdir-manifest /tmp/snapdir-file-store_tests/files
snapdir-manifest /tmp/snapdir-manifest_tests/guide-files
snapdir-manifest /tmp/snapdir_tests/files
snapdir-manifest test
