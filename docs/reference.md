# snapdir reference

  - [Table of contents](#table-of-contents)
    - [snapdir](#snapdir)
      - [snapdir manifest](#snapdir-manifest)
      - [snapdir id](#snapdir-id)
      - [snapdir push](#snapdir-push)
      - [snapdir fetch](#snapdir-fetch)
      - [snapdir pull](#snapdir-pull)
      - [snapdir checkout](#snapdir-checkout)
      - [snapdir stage](#snapdir-stage)
      - [snapdir verify](#snapdir-verify)
      - [snapdir verify-cache](#snapdir-verify-cache)
      - [snapdir flush-cache](#snapdir-flush-cache)
      - [snapdir defaults](#snapdir-defaults)
      - [snapdir test](#snapdir-test)
    - [snapdir-b2-store](#snapdir-b2-store)
      - [snapdir-b2-store get-push-command](#snapdir-b2-store-get-push-command)
      - [snapdir-b2-store get-manifest-command](#snapdir-b2-store-get-manifest-command)
      - [snapdir-b2-store get-fetch-files-command](#snapdir-b2-store-get-fetch-files-command)
      - [snapdir-b2-store get-manifest](#snapdir-b2-store-get-manifest)
      - [snapdir-b2-store fetch](#snapdir-b2-store-fetch)
      - [snapdir-b2-store ensure-no-errors](#snapdir-b2-store-ensure-no-errors)
      - [snapdir-b2-store test](#snapdir-b2-store-test)
    - [snapdir-file-store](#snapdir-file-store)
      - [snapdir-file-store get-push-command](#snapdir-file-store-get-push-command)
      - [snapdir-file-store get-manifest-command](#snapdir-file-store-get-manifest-command)
      - [snapdir-file-store get-fetch-files-command](#snapdir-file-store-get-fetch-files-command)
      - [snapdir-file-store ensure-no-errors](#snapdir-file-store-ensure-no-errors)
      - [snapdir-file-store commit-manifest](#snapdir-file-store-commit-manifest)
      - [snapdir-file-store fetch-object](#snapdir-file-store-fetch-object)
      - [snapdir-file-store commit-object](#snapdir-file-store-commit-object)
      - [snapdir-file-store test](#snapdir-file-store-test)
    - [snapdir-manifest](#snapdir-manifest)
      - [snapdir-manifest (generate)](#snapdir-manifest-(generate))
      - [snapdir-manifest flush-cache](#snapdir-manifest-flush-cache)
      - [snapdir-manifest cache-id](#snapdir-manifest-cache-id)
      - [snapdir-manifest defaults](#snapdir-manifest-defaults)
      - [snapdir-manifest test](#snapdir-manifest-test)
    - [snapdir-s3-store](#snapdir-s3-store)
      - [snapdir-s3-store get-push-command](#snapdir-s3-store-get-push-command)
      - [snapdir-s3-store get-manifest-command](#snapdir-s3-store-get-manifest-command)
      - [snapdir-s3-store get-fetch-files-command](#snapdir-s3-store-get-fetch-files-command)
      - [snapdir-s3-store get-manifest](#snapdir-s3-store-get-manifest)
      - [snapdir-s3-store fetch](#snapdir-s3-store-fetch)
      - [snapdir-s3-store ensure-no-errors](#snapdir-s3-store-ensure-no-errors)
      - [snapdir-s3-store test](#snapdir-s3-store-test)


## snapdir

### snapdir manifest

[snapdir](#snapdir) [manifest](#manifest) [toc](#snapdir-reference)


Examples from tests:

```bash
snapdir manifest "${DIR}"
```
```bash
snapdir manifest --exclude "${EXCLUDE_PATTERN}" "${DIR}"
```

### snapdir id

[snapdir](#snapdir) [id](#id) [toc](#snapdir-reference)


Examples from tests:

```bash
snapdir id "${DIR}"
```

### snapdir push

[snapdir](#snapdir) [push](#push) [toc](#snapdir-reference)


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

[snapdir](#snapdir) [fetch](#fetch) [toc](#snapdir-reference)


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

[snapdir](#snapdir) [pull](#pull) [toc](#snapdir-reference)


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

[snapdir](#snapdir) [checkout](#checkout) [toc](#snapdir-reference)


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

[snapdir](#snapdir) [stage](#stage) [toc](#snapdir-reference)


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

[snapdir](#snapdir) [verify](#verify) [toc](#snapdir-reference)


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

[snapdir](#snapdir) [verify-cache](#verify-cache) [toc](#snapdir-reference)


Examples from tests:

```bash
snapdir verify-cache
```
```bash
snapdir verify-cache --purge
```

### snapdir flush-cache

[snapdir](#snapdir) [flush-cache](#flush-cache) [toc](#snapdir-reference)


Examples from tests:

```bash
snapdir flush-cache
```

### snapdir defaults

[snapdir](#snapdir) [defaults](#defaults) [toc](#snapdir-reference)


No examples found on docs/tests/tested-commands.sh

### snapdir test

[snapdir](#snapdir) [test](#test) [toc](#snapdir-reference)


Examples from tests:

```bash
snapdir test
```


## snapdir-b2-store

### snapdir-b2-store get-push-command

[snapdir-b2-store](#snapdir-b2-store) [get-push-command](#get-push-command) [toc](#snapdir-reference)


Examples from tests:

```bash
snapdir-b2-store get-push-command --id "${ID}" --staging-dir "${STAGING_DIR}"
```

### snapdir-b2-store get-manifest-command

[snapdir-b2-store](#snapdir-b2-store) [get-manifest-command](#get-manifest-command) [toc](#snapdir-reference)


Examples from tests:

```bash
snapdir-b2-store get-manifest-command --id "${ID}" --store "${STORE}"
```
```bash
snapdir-b2-store get-manifest-command --id bogus --store "${STORE}"
```

### snapdir-b2-store get-fetch-files-command

[snapdir-b2-store](#snapdir-b2-store) [get-fetch-files-command](#get-fetch-files-command) [toc](#snapdir-reference)


Examples from tests:

```bash
snapdir-b2-store get-fetch-files-command --id "${ID}" --store "${STORE}" "${DIR}"
```

### snapdir-b2-store get-manifest

[snapdir-b2-store](#snapdir-b2-store) [get-manifest](#get-manifest) [toc](#snapdir-reference)


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

[snapdir-b2-store](#snapdir-b2-store) [fetch](#fetch) [toc](#snapdir-reference)


Examples from tests:

```bash
snapdir-b2-store fetch --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --log-file "${LOG_PATH}"
```
```bash
snapdir-b2-store fetch --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --store "${STORE}"
```

### snapdir-b2-store ensure-no-errors

[snapdir-b2-store](#snapdir-b2-store) [ensure-no-errors](#ensure-no-errors) [toc](#snapdir-reference)


Examples from tests:

```bash
snapdir-b2-store ensure-no-errors --checksum "${ID}" --log-file "${LOG_PATH}"
```

### snapdir-b2-store test

[snapdir-b2-store](#snapdir-b2-store) [test](#test) [toc](#snapdir-reference)


Examples from tests:

```bash
snapdir-b2-store test --store "${STORE}"
```


## snapdir-file-store

### snapdir-file-store get-push-command

[snapdir-file-store](#snapdir-file-store) [get-push-command](#get-push-command) [toc](#snapdir-reference)


Examples from tests:

```bash
snapdir-file-store get-push-command --id "${ID}" --staging-dir "${STAGING_DIR}"
```

### snapdir-file-store get-manifest-command

[snapdir-file-store](#snapdir-file-store) [get-manifest-command](#get-manifest-command) [toc](#snapdir-reference)


Examples from tests:

```bash
snapdir-file-store get-manifest-command --id "${ID}" --store "${STORE}"
```
```bash
snapdir-file-store get-manifest-command --id bogus --store "${STORE}"
```

### snapdir-file-store get-fetch-files-command

[snapdir-file-store](#snapdir-file-store) [get-fetch-files-command](#get-fetch-files-command) [toc](#snapdir-reference)


Examples from tests:

```bash
snapdir-file-store get-fetch-files-command --id "${ID}" --store "${STORE}" --cache-dir "${CACHE_DIR}"
```

### snapdir-file-store ensure-no-errors

[snapdir-file-store](#snapdir-file-store) [ensure-no-errors](#ensure-no-errors) [toc](#snapdir-reference)


Examples from tests:

```bash
snapdir-file-store ensure-no-errors --checksum "${ID}" --log-file "${LOG_PATH}"
```

### snapdir-file-store commit-manifest

[snapdir-file-store](#snapdir-file-store) [commit-manifest](#commit-manifest) [toc](#snapdir-reference)


Examples from tests:

```bash
snapdir-file-store commit-manifest --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --log-file "${LOG_PATH}"
```

### snapdir-file-store fetch-object

[snapdir-file-store](#snapdir-file-store) [fetch-object](#fetch-object) [toc](#snapdir-reference)


Examples from tests:

```bash
snapdir-file-store fetch-object --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --log-file "${LOG_PATH}"
```

### snapdir-file-store commit-object

[snapdir-file-store](#snapdir-file-store) [commit-object](#commit-object) [toc](#snapdir-reference)


Examples from tests:

```bash
snapdir-file-store commit-object --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --log-file "${LOG_PATH}"
```

### snapdir-file-store test

[snapdir-file-store](#snapdir-file-store) [test](#test) [toc](#snapdir-reference)


Examples from tests:

```bash
snapdir-file-store test
```


## snapdir-manifest

### snapdir-manifest (generate)

[snapdir-manifest](#snapdir-manifest) [generate](#generate) [toc](#snapdir-reference)


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

### snapdir-manifest flush-cache

[snapdir-manifest](#snapdir-manifest) [flush-cache](#flush-cache) [toc](#snapdir-reference)


No examples found on docs/tests/tested-commands.sh

### snapdir-manifest cache-id

[snapdir-manifest](#snapdir-manifest) [cache-id](#cache-id) [toc](#snapdir-reference)


No examples found on docs/tests/tested-commands.sh

### snapdir-manifest defaults

[snapdir-manifest](#snapdir-manifest) [defaults](#defaults) [toc](#snapdir-reference)


No examples found on docs/tests/tested-commands.sh

### snapdir-manifest test

[snapdir-manifest](#snapdir-manifest) [test](#test) [toc](#snapdir-reference)


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

[snapdir-s3-store](#snapdir-s3-store) [get-push-command](#get-push-command) [toc](#snapdir-reference)


Examples from tests:

```bash
snapdir-s3-store get-push-command --id "${ID}" --staging-dir "${STAGING_DIR}"
```

### snapdir-s3-store get-manifest-command

[snapdir-s3-store](#snapdir-s3-store) [get-manifest-command](#get-manifest-command) [toc](#snapdir-reference)


Examples from tests:

```bash
snapdir-s3-store get-manifest-command --id "${ID}" --store "${STORE}"
```
```bash
snapdir-s3-store get-manifest-command --id bogus --store "${STORE}"
```

### snapdir-s3-store get-fetch-files-command

[snapdir-s3-store](#snapdir-s3-store) [get-fetch-files-command](#get-fetch-files-command) [toc](#snapdir-reference)


Examples from tests:

```bash
snapdir-s3-store get-fetch-files-command --id "${ID}" --store "${STORE}" "${DIR}"
```

### snapdir-s3-store get-manifest

[snapdir-s3-store](#snapdir-s3-store) [get-manifest](#get-manifest) [toc](#snapdir-reference)


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

[snapdir-s3-store](#snapdir-s3-store) [fetch](#fetch) [toc](#snapdir-reference)


Examples from tests:

```bash
snapdir-s3-store fetch --store "${STORE}" --target-path "${TARGET_PATH}" --log-file "${LOG_PATH}"
```
```bash
snapdir-s3-store fetch --store "${STORE}" --target-path "${TARGET_PATH}" --store "${STORE}"
```

### snapdir-s3-store ensure-no-errors

[snapdir-s3-store](#snapdir-s3-store) [ensure-no-errors](#ensure-no-errors) [toc](#snapdir-reference)


Examples from tests:

```bash
snapdir-s3-store ensure-no-errors --store "${STORE}" "${DIR}"
```

### snapdir-s3-store test

[snapdir-s3-store](#snapdir-s3-store) [test](#test) [toc](#snapdir-reference)


Examples from tests:

```bash
snapdir-s3-store test --store "${STORE}"
```
