# snapdir reference


## snapdir

### snapdir run

No examples found on docs/tests/tested-commands.sh

### snapdir manifest

Examples from tests:

```bash
snapdir manifest "${DIR}"
```
```bash
snapdir manifest --exclude "${EXCLUDE_PATTERN}" "${DIR}"
```

### snapdir id

Examples from tests:

```bash
snapdir id "${DIR}"
```

### snapdir push

Examples from tests:

```bash
snapdir push --id "${ID}" --debug --verbose --store "${STORE}"
```
```bash
snapdir push --id "${ID}" --store "${STORE}"
```
```bash
snapdir push --id "${ID}" --verbose --store "${STORE}"
```
```bash
snapdir push --verbose --dryrun --store "${STORE}" "${DIR}"
```
```bash
snapdir push --verbose --store "${STORE}" "${DIR}"
```

### snapdir fetch

Examples from tests:

```bash
snapdir fetch --dryrun --store "${STORE}"
```
```bash
snapdir fetch --dryrun --store "${STORE}" --id "${ID}"
```
```bash
snapdir fetch --store "${STORE}"
```
```bash
snapdir fetch --store "${STORE}" --id "${ID}" --verbose
```
```bash
snapdir fetch --store "${STORE}" --id bogus --verbose
```

### snapdir pull

Examples from tests:

```bash
snapdir pull --dryrun --verbose --store "${STORE}" "${DIR}"
```
```bash
snapdir pull --dryrun --verbose --store "${STORE}" --id "${ID}" "${DIR}"
```
```bash
snapdir pull --verbose --store "${STORE}" "${DIR}"
```
```bash
snapdir pull --verbose --store "${STORE}" --id "${ID}" "${DIR}"
```

### snapdir checkout

Examples from tests:

```bash
snapdir checkout --force --id "${ID}" "${DIR}"
```
```bash
snapdir checkout --verbose --force --id "${ID}" "${DIR}"
```
```bash
snapdir checkout --verbose --id "${ID}" "${DIR}"
```
```bash
snapdir checkout --verbose --id "${ID}" --linked "${DIR}"
```
```bash
snapdir checkout --verbose --id "${ID}" --path "${PATH}" "${DIR}"
```

### snapdir stage

Examples from tests:

```bash
snapdir stage "${DIR}"
```
```bash
snapdir stage --keep "${DIR}"
```
```bash
snapdir stage --verbose "${DIR}"
```

### snapdir verify

Examples from tests:

```bash
snapdir verify --verbose --id "${ID}"
```
```bash
snapdir verify --verbose --purge --id "${ID}"
```
```bash
snapdir verify-cache
```
```bash
snapdir verify-cache --purge
```

### snapdir verify-cache

Examples from tests:

```bash
snapdir verify-cache
```
```bash
snapdir verify-cache --purge
```

### snapdir flush-cache

Examples from tests:

```bash
snapdir flush-cache
```

### snapdir defaults

No examples found on docs/tests/tested-commands.sh

### snapdir test

Examples from tests:

```bash
snapdir test
```


## snapdir-b2-store

### snapdir-b2-store get-push-command

Examples from tests:

```bash
snapdir-b2-store get-push-command --id "${ID}" --staging-dir "${STAGING_DIR}"
```

### snapdir-b2-store get-manifest-command

Examples from tests:

```bash
snapdir-b2-store get-manifest-command --id "${ID}" --store "${STORE}"
```
```bash
snapdir-b2-store get-manifest-command --id bogus --store "${STORE}"
```

### snapdir-b2-store get-fetch-files-command

Examples from tests:

```bash
snapdir-b2-store get-fetch-files-command --id "${ID}" --store "${STORE}" "${DIR}"
```

### snapdir-b2-store get-manifest

Examples from tests:

```bash
snapdir-b2-store get-manifest --id "${ID}" --store "${STORE}"
```
```bash
snapdir-b2-store get-manifest-command --id "${ID}" --store "${STORE}"
```
```bash
snapdir-b2-store get-manifest-command --id bogus --store "${STORE}"
```

### snapdir-b2-store fetch

Examples from tests:

```bash
snapdir-b2-store fetch --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --log-file "${LOG_PATH}"
```
```bash
snapdir-b2-store fetch --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --store "${STORE}"
```

### snapdir-b2-store ensure-no-errors

Examples from tests:

```bash
snapdir-b2-store ensure-no-errors --checksum "${ID}" --log-file "${LOG_PATH}"
```

### snapdir-b2-store run

No examples found on docs/tests/tested-commands.sh

### snapdir-b2-store test

Examples from tests:

```bash
snapdir-b2-store test --store "${STORE}"
```


## snapdir-file-store

### snapdir-file-store get-push-command

Examples from tests:

```bash
snapdir-file-store get-push-command --id "${ID}" --staging-dir "${STAGING_DIR}"
```

### snapdir-file-store get-manifest-command

Examples from tests:

```bash
snapdir-file-store get-manifest-command --id "${ID}" --store "${STORE}"
```
```bash
snapdir-file-store get-manifest-command --id bogus --store "${STORE}"
```

### snapdir-file-store get-fetch-files-command

Examples from tests:

```bash
snapdir-file-store get-fetch-files-command --id "${ID}" --store "${STORE}" --cache-dir "${CACHE_DIR}"
```

### snapdir-file-store ensure-no-errors

Examples from tests:

```bash
snapdir-file-store ensure-no-errors --checksum "${ID}" --log-file "${LOG_PATH}"
```

### snapdir-file-store commit-manifest

Examples from tests:

```bash
snapdir-file-store commit-manifest --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --log-file "${LOG_PATH}"
```

### snapdir-file-store fetch-object

Examples from tests:

```bash
snapdir-file-store fetch-object --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --log-file "${LOG_PATH}"
```

### snapdir-file-store commit-object

Examples from tests:

```bash
snapdir-file-store commit-object --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --log-file "${LOG_PATH}"
```

### snapdir-file-store run

No examples found on docs/tests/tested-commands.sh

### snapdir-file-store test

Examples from tests:

```bash
snapdir-file-store test
```


## snapdir-manifest

### snapdir-manifest [generate]

Examples from tests:

```bash
snapdir-manifest --checksum-bin b3sum "${DIR}"
```
```bash
snapdir-manifest --checksum-bin b3sum --absolute "${DIR}"
```
```bash
snapdir-manifest --checksum-bin b3sum --cache --cache-id "${ID}" --cache-dir "${CACHE_DIR}" "${DIR}"
```
```bash
snapdir-manifest --checksum-bin b3sum --cache --cache-id bogus --cache-dir "${CACHE_DIR}" "${DIR}"
```
```bash
snapdir-manifest --checksum-bin b3sum --cache --verbose --cache-dir "${CACHE_DIR}" "${DIR}"
```
```bash
snapdir-manifest --checksum-bin b3sum --exclude "${EXCLUDE_PATTERN}" "${DIR}"
```
```bash
snapdir-manifest --checksum-bin b3sum cache-id --cache-dir "${CACHE_DIR}"
```
```bash
snapdir-manifest --checksum-bin md5sum "${DIR}"
```
```bash
snapdir-manifest --checksum-bin md5sum --absolute "${DIR}"
```
```bash
snapdir-manifest --checksum-bin md5sum --cache --cache-id "${ID}" --cache-dir "${CACHE_DIR}" "${DIR}"
```
```bash
snapdir-manifest --checksum-bin md5sum --cache --cache-id bogus --cache-dir "${CACHE_DIR}" "${DIR}"
```
```bash
snapdir-manifest --checksum-bin md5sum --cache --verbose --cache-dir "${CACHE_DIR}" "${DIR}"
```
```bash
snapdir-manifest --checksum-bin md5sum --exclude "${EXCLUDE_PATTERN}" "${DIR}"
```
```bash
snapdir-manifest --checksum-bin md5sum cache-id --cache-dir "${CACHE_DIR}"
```
```bash
snapdir-manifest --checksum-bin sha256sum "${DIR}"
```
```bash
snapdir-manifest --checksum-bin sha256sum --absolute "${DIR}"
```
```bash
snapdir-manifest --checksum-bin sha256sum --cache --cache-id "${ID}" --cache-dir "${CACHE_DIR}" "${DIR}"
```
```bash
snapdir-manifest --checksum-bin sha256sum --cache --cache-id bogus --cache-dir "${CACHE_DIR}" "${DIR}"
```
```bash
snapdir-manifest --checksum-bin sha256sum --cache --verbose --cache-dir "${CACHE_DIR}" "${DIR}"
```
```bash
snapdir-manifest --checksum-bin sha256sum --exclude "${EXCLUDE_PATTERN}" "${DIR}"
```
```bash
snapdir-manifest --checksum-bin sha256sum cache-id --cache-dir "${CACHE_DIR}"
```
```bash
snapdir-manifest --exclude "${EXCLUDE_PATTERN}" "${DIR}"
```
```bash
snapdir-manifest --no-follow --checksum-bin b3sum "${DIR}"
```
```bash
snapdir-manifest --no-follow --checksum-bin md5sum "${DIR}"
```
```bash
snapdir-manifest --no-follow --checksum-bin sha256sum "${DIR}"
```

### snapdir-manifest run

No examples found on docs/tests/tested-commands.sh

### snapdir-manifest flush-cache

No examples found on docs/tests/tested-commands.sh

### snapdir-manifest cache-id

No examples found on docs/tests/tested-commands.sh

### snapdir-manifest defaults

No examples found on docs/tests/tested-commands.sh

### snapdir-manifest test

Examples from tests:

```bash
snapdir-manifest test
```
```bash
snapdir-manifest test --checksum-bin md5sum
```
```bash
snapdir-manifest test --checksum-bin sha256sum
```


## snapdir-s3-store

### snapdir-s3-store get-push-command

Examples from tests:

```bash
snapdir-s3-store get-push-command --id "${ID}" --staging-dir "${STAGING_DIR}"
```

### snapdir-s3-store get-manifest-command

Examples from tests:

```bash
snapdir-s3-store get-manifest-command --id "${ID}" --store "${STORE}"
```
```bash
snapdir-s3-store get-manifest-command --id bogus --store "${STORE}"
```

### snapdir-s3-store get-fetch-files-command

Examples from tests:

```bash
snapdir-s3-store get-fetch-files-command --id "${ID}" --store "${STORE}" "${DIR}"
```

### snapdir-s3-store get-manifest

Examples from tests:

```bash
snapdir-s3-store get-manifest --id "${ID}" --store "${STORE}"
```
```bash
snapdir-s3-store get-manifest-command --id "${ID}" --store "${STORE}"
```
```bash
snapdir-s3-store get-manifest-command --id bogus --store "${STORE}"
```

### snapdir-s3-store fetch

Examples from tests:

```bash
snapdir-s3-store fetch --store "${STORE}" --target-path "${TARGET_PATH}" --log-file "${LOG_PATH}"
```
```bash
snapdir-s3-store fetch --store "${STORE}" --target-path "${TARGET_PATH}" --store "${STORE}"
```

### snapdir-s3-store ensure-no-errors

Examples from tests:

```bash
snapdir-s3-store ensure-no-errors --store "${STORE}" "${DIR}"
```

### snapdir-s3-store run

No examples found on docs/tests/tested-commands.sh

### snapdir-s3-store test

Examples from tests:

```bash
snapdir-s3-store test --store "${STORE}"
```

