#!/usr/bin/env bash
# WARNING, do not edit manually.
# generated by running:
# _SNAPDIR_RUN_LOG_PATH="$(pwd)/docs/tests/tested-commands.sh" ./snapdir-test integration
# We use the results to generate documentation and generative testing.

snapdir-manifest test
snapdir-manifest --checksum-bin b3sum "${DIR}"
snapdir-manifest --checksum-bin b3sum --exclude "${EXCLUDE_PATTERN}" "${DIR}"
snapdir-manifest --checksum-bin b3sum --exclude "${EXCLUDE_PATTERN}" "${DIR}"
snapdir-manifest --checksum-bin b3sum "${DIR}"
snapdir-manifest --checksum-bin b3sum --absolute "${DIR}"
snapdir-manifest --checksum-bin b3sum "${DIR}"
snapdir-manifest --checksum-bin b3sum --cache --verbose --cache-dir "${CACHE_DIR}" "${DIR}"
snapdir-manifest --checksum-bin b3sum --cache --verbose --cache-dir "${CACHE_DIR}" "${DIR}"
snapdir-manifest --checksum-bin b3sum cache-id --cache-dir "${CACHE_DIR}"
snapdir-manifest --checksum-bin b3sum --cache --cache-id "${ID}" --cache-dir "${CACHE_DIR}" "${DIR}"
snapdir-manifest --checksum-bin b3sum --cache --cache-id bogus --cache-dir "${CACHE_DIR}" "${DIR}"
snapdir-manifest --checksum-bin b3sum "${DIR}"
snapdir-manifest --no-follow --checksum-bin b3sum "${DIR}"
snapdir-manifest --checksum-bin b3sum "${DIR}"
snapdir-manifest "${DIR}"
snapdir-manifest test --checksum-bin md5sum
snapdir-manifest --checksum-bin md5sum "${DIR}"
snapdir-manifest --checksum-bin md5sum --exclude "${EXCLUDE_PATTERN}" "${DIR}"
snapdir-manifest --checksum-bin md5sum --exclude "${EXCLUDE_PATTERN}" "${DIR}"
snapdir-manifest --checksum-bin md5sum "${DIR}"
snapdir-manifest --checksum-bin md5sum --absolute "${DIR}"
snapdir-manifest --checksum-bin md5sum "${DIR}"
snapdir-manifest --checksum-bin md5sum --cache --verbose --cache-dir "${CACHE_DIR}" "${DIR}"
snapdir-manifest --checksum-bin md5sum --cache --verbose --cache-dir "${CACHE_DIR}" "${DIR}"
snapdir-manifest --checksum-bin md5sum cache-id --cache-dir "${CACHE_DIR}"
snapdir-manifest --checksum-bin md5sum --cache --cache-id "${ID}" --cache-dir "${CACHE_DIR}" "${DIR}"
snapdir-manifest --checksum-bin md5sum --cache --cache-id bogus --cache-dir "${CACHE_DIR}" "${DIR}"
snapdir-manifest --checksum-bin md5sum "${DIR}"
snapdir-manifest --no-follow --checksum-bin md5sum "${DIR}"
snapdir-manifest --checksum-bin md5sum "${DIR}"
snapdir-manifest "${DIR}"
snapdir-manifest test --checksum-bin sha256sum
snapdir-manifest --checksum-bin sha256sum "${DIR}"
snapdir-manifest --checksum-bin sha256sum --exclude "${EXCLUDE_PATTERN}" "${DIR}"
snapdir-manifest --checksum-bin sha256sum --exclude "${EXCLUDE_PATTERN}" "${DIR}"
snapdir-manifest --checksum-bin sha256sum "${DIR}"
snapdir-manifest --checksum-bin sha256sum --absolute "${DIR}"
snapdir-manifest --checksum-bin sha256sum "${DIR}"
snapdir-manifest --checksum-bin sha256sum --cache --verbose --cache-dir "${CACHE_DIR}" "${DIR}"
snapdir-manifest --checksum-bin sha256sum --cache --verbose --cache-dir "${CACHE_DIR}" "${DIR}"
snapdir-manifest --checksum-bin sha256sum cache-id --cache-dir "${CACHE_DIR}"
snapdir-manifest --checksum-bin sha256sum --cache --cache-id "${ID}" --cache-dir "${CACHE_DIR}" "${DIR}"
snapdir-manifest --checksum-bin sha256sum --cache --cache-id bogus --cache-dir "${CACHE_DIR}" "${DIR}"
snapdir-manifest --checksum-bin sha256sum "${DIR}"
snapdir-manifest --no-follow --checksum-bin sha256sum "${DIR}"
snapdir-manifest --checksum-bin sha256sum "${DIR}"
snapdir-manifest "${DIR}"
snapdir test
snapdir manifest "${DIR}"
snapdir-manifest "${DIR}"
snapdir manifest --exclude "${EXCLUDE_PATTERN}" "${DIR}"
snapdir-manifest --exclude "${EXCLUDE_PATTERN}" "${DIR}"
snapdir id "${DIR}"
snapdir-manifest "${DIR}"
snapdir id "${DIR}"
snapdir-manifest "${DIR}"
snapdir id "${DIR}"
snapdir id "${DIR}"
snapdir stage --keep "${DIR}"
snapdir-manifest "${DIR}"
snapdir-manifest "${DIR}"
snapdir stage "${DIR}"
snapdir-manifest "${DIR}"
snapdir-manifest "${DIR}"
snapdir manifest "${DIR}"
snapdir-manifest "${DIR}"
snapdir stage --keep "${DIR}"
snapdir-manifest "${DIR}"
snapdir-manifest "${DIR}"
snapdir stage --keep "${DIR}"
snapdir-manifest "${DIR}"
snapdir-manifest "${DIR}"
snapdir stage --keep "${DIR}"
snapdir-manifest "${DIR}"
snapdir-manifest "${DIR}"
snapdir stage "${DIR}"
snapdir-manifest "${DIR}"
snapdir-manifest "${DIR}"
snapdir stage "${DIR}"
snapdir-manifest "${DIR}"
snapdir-manifest "${DIR}"
snapdir id "${DIR}"
snapdir checkout --verbose --id "${ID}" "${DIR}"
snapdir checkout --verbose --id "${ID}" "${DIR}"
snapdir id "${DIR}"
snapdir-manifest "${DIR}"
snapdir checkout --verbose --id "${ID}" "${DIR}"
snapdir checkout --verbose --force --id "${ID}" "${DIR}"
snapdir checkout --verbose --id "${ID}" "${DIR}"
snapdir checkout --verbose --id "${ID}" --path "${PATH}" "${DIR}"
snapdir checkout --verbose --id "${ID}" --path "${PATH}" "${DIR}"
snapdir checkout --verbose --id "${ID}" --path "${PATH}" "${DIR}"
snapdir checkout --verbose --id "${ID}" --path "${PATH}" "${DIR}"
snapdir checkout --verbose --id "${ID}" --linked "${DIR}"
snapdir id "${DIR}"
snapdir-manifest "${DIR}"
snapdir checkout --verbose --id "${ID}" "${DIR}"
snapdir verify --verbose --id "${ID}"
snapdir verify --verbose --id "${ID}"
snapdir verify --verbose --id "${ID}"
snapdir verify --verbose --purge --id "${ID}"
snapdir stage --verbose "${DIR}"
snapdir-manifest "${DIR}"
snapdir-manifest "${DIR}"
snapdir verify --verbose --id "${ID}"
snapdir verify-cache
snapdir verify-cache --purge
snapdir stage "${DIR}"
snapdir-manifest "${DIR}"
snapdir-manifest "${DIR}"
snapdir checkout --force --id "${ID}" "${DIR}"
snapdir checkout --force --id "${ID}" "${DIR}"
snapdir flush-cache
snapdir flush-cache
snapdir checkout --verbose --id "${ID}" "${DIR}"
snapdir version
snapdir-file-store test
snapdir stage "${DIR}"
snapdir-manifest "${DIR}"
snapdir-manifest "${DIR}"
snapdir push --verbose --dryrun --store "${STORE}" "${DIR}"
snapdir-manifest "${DIR}"
snapdir-manifest "${DIR}"
snapdir-manifest "${DIR}"
snapdir-file-store get-push-command --id "${ID}" --staging-dir "${STAGING_DIR}"
snapdir push --id "${ID}" --debug --verbose --store "${STORE}"
snapdir-file-store get-push-command --id "${ID}" --staging-dir "${STAGING_DIR}"
snapdir-file-store commit-manifest --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --log-file "${LOG_PATH}"
snapdir-file-store commit-object --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --log-file "${LOG_PATH}"
snapdir-file-store commit-object --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --log-file "${LOG_PATH}"
snapdir push --id "${ID}" --verbose --store "${STORE}"
snapdir-file-store get-push-command --id "${ID}" --staging-dir "${STAGING_DIR}"
snapdir push --id "${ID}" --verbose --store "${STORE}"
snapdir-file-store get-push-command --id "${ID}" --staging-dir "${STAGING_DIR}"
snapdir-file-store commit-manifest --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --log-file "${LOG_PATH}"
snapdir push --id "${ID}" --verbose --store "${STORE}"
snapdir-file-store get-push-command --id "${ID}" --staging-dir "${STAGING_DIR}"
snapdir-file-store commit-object --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --log-file "${LOG_PATH}"
snapdir-file-store commit-manifest --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --log-file "${LOG_PATH}"
snapdir push --id "${ID}" --verbose --store "${STORE}"
snapdir-file-store get-push-command --id "${ID}" --staging-dir "${STAGING_DIR}"
snapdir-file-store commit-manifest --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --log-file "${LOG_PATH}"
snapdir-file-store commit-object --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --log-file "${LOG_PATH}"
snapdir fetch --dryrun --store "${STORE}" --id "${ID}"
snapdir-file-store get-manifest-command --id "${ID}" --store "${STORE}"
snapdir-file-store get-fetch-files-command --id "${ID}" --store "${STORE}" --cache-dir "${CACHE_DIR}"
snapdir fetch --dryrun --store "${STORE}" --id "${ID}"
snapdir-file-store get-manifest-command --id "${ID}" --store "${STORE}"
snapdir fetch --store "${STORE}" --id "${ID}" --verbose
snapdir-file-store get-manifest-command --id "${ID}" --store "${STORE}"
snapdir-file-store get-fetch-files-command --id "${ID}" --store "${STORE}" --cache-dir "${CACHE_DIR}"
snapdir-file-store ensure-no-errors --checksum "${ID}" --log-file "${LOG_PATH}"
snapdir-file-store fetch-object --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --log-file "${LOG_PATH}"
snapdir-file-store fetch-object --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --log-file "${LOG_PATH}"
snapdir fetch --store "${STORE}" --id bogus --verbose
snapdir-file-store get-manifest-command --id bogus --store "${STORE}"
snapdir pull --dryrun --verbose --store "${STORE}" --id "${ID}" "${DIR}"
snapdir-file-store get-fetch-files-command --id "${ID}" --store "${STORE}" --cache-dir "${CACHE_DIR}"
snapdir pull --verbose --store "${STORE}" --id "${ID}" "${DIR}"
snapdir-file-store get-fetch-files-command --id "${ID}" --store "${STORE}" --cache-dir "${CACHE_DIR}"
snapdir-file-store ensure-no-errors --checksum "${ID}" --log-file "${LOG_PATH}"
snapdir pull --verbose --store "${STORE}" --id "${ID}" "${DIR}"
snapdir-file-store get-manifest-command --id "${ID}" --store "${STORE}"
snapdir-file-store get-fetch-files-command --id "${ID}" --store "${STORE}" --cache-dir "${CACHE_DIR}"
snapdir-file-store fetch-object --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --log-file "${LOG_PATH}"
snapdir-file-store fetch-object --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --log-file "${LOG_PATH}"
snapdir-file-store ensure-no-errors --checksum "${ID}" --log-file "${LOG_PATH}"
snapdir-b2-store test --store "${STORE}"
snapdir stage "${DIR}"
snapdir-manifest "${DIR}"
snapdir-manifest "${DIR}"
snapdir push --verbose --dryrun --store "${STORE}" "${DIR}"
snapdir-manifest "${DIR}"
snapdir-manifest "${DIR}"
snapdir-manifest "${DIR}"
snapdir-b2-store get-push-command --id "${ID}" --staging-dir "${STAGING_DIR}"
snapdir push --verbose --store "${STORE}" "${DIR}"
snapdir-manifest "${DIR}"
snapdir-manifest "${DIR}"
snapdir-manifest "${DIR}"
snapdir-b2-store get-push-command --id "${ID}" --staging-dir "${STAGING_DIR}"
snapdir-b2-store get-manifest --id "${ID}" --store "${STORE}"
snapdir-b2-store fetch --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --store "${STORE}"
snapdir-b2-store get-manifest --id "${ID}" --store "${STORE}"
snapdir-b2-store fetch --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --store "${STORE}"
snapdir-b2-store ensure-no-errors --checksum "${ID}" --log-file "${LOG_PATH}"
snapdir push --id "${ID}" --verbose --store "${STORE}"
snapdir-b2-store get-push-command --id "${ID}" --staging-dir "${STAGING_DIR}"
snapdir-b2-store get-manifest --id "${ID}" --store "${STORE}"
snapdir-b2-store fetch --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --store "${STORE}"
snapdir-b2-store ensure-no-errors --checksum "${ID}" --log-file "${LOG_PATH}"
snapdir push --id "${ID}" --store "${STORE}"
snapdir-b2-store get-push-command --id "${ID}" --staging-dir "${STAGING_DIR}"
snapdir-b2-store get-manifest --id "${ID}" --store "${STORE}"
snapdir-b2-store fetch --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --store "${STORE}"
snapdir-b2-store ensure-no-errors --checksum "${ID}" --log-file "${LOG_PATH}"
snapdir fetch --dryrun --store "${STORE}"
snapdir-b2-store get-manifest-command --id "${ID}" --store "${STORE}"
snapdir-b2-store get-manifest --id "${ID}" --store "${STORE}"
snapdir-b2-store fetch --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --store "${STORE}"
snapdir-b2-store ensure-no-errors --checksum "${ID}" --log-file "${LOG_PATH}"
snapdir-b2-store get-fetch-files-command --id "${ID}" --store "${STORE}" "${DIR}"
snapdir fetch --store "${STORE}"
snapdir-b2-store get-manifest-command --id "${ID}" --store "${STORE}"
snapdir-b2-store get-manifest --id "${ID}" --store "${STORE}"
snapdir-b2-store fetch --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --store "${STORE}"
snapdir-b2-store ensure-no-errors --checksum "${ID}" --log-file "${LOG_PATH}"
snapdir-b2-store get-fetch-files-command --id "${ID}" --store "${STORE}" "${DIR}"
snapdir-b2-store fetch --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --log-file "${LOG_PATH}"
snapdir-b2-store ensure-no-errors --checksum "${ID}" --log-file "${LOG_PATH}"
snapdir-b2-store fetch --checksum "${ID}" --source-path "${SOURCE_PATH}" --target-path "${TARGET_PATH}" --log-file "${LOG_PATH}"
snapdir fetch --store "${STORE}"
snapdir-b2-store get-manifest-command --id bogus --store "${STORE}"
snapdir fetch --dryrun --store "${STORE}"
snapdir pull --dryrun --verbose --store "${STORE}" "${DIR}"
snapdir-b2-store get-fetch-files-command --id "${ID}" --store "${STORE}" "${DIR}"
snapdir pull --verbose --store "${STORE}" "${DIR}"
snapdir-b2-store get-fetch-files-command --id "${ID}" --store "${STORE}" "${DIR}"
snapdir-b2-store ensure-no-errors --checksum "${ID}" --log-file "${LOG_PATH}"
snapdir-s3-store test --store "${STORE}"
snapdir stage "${DIR}"
snapdir-manifest "${DIR}"
snapdir-manifest "${DIR}"
snapdir push --verbose --dryrun --store "${STORE}" "${DIR}"
snapdir-manifest "${DIR}"
snapdir-manifest "${DIR}"
snapdir-manifest "${DIR}"
snapdir-s3-store get-push-command --id "${ID}" --staging-dir "${STAGING_DIR}"
snapdir push --verbose --store "${STORE}" "${DIR}"
snapdir-manifest "${DIR}"
snapdir-manifest "${DIR}"
snapdir-manifest "${DIR}"
snapdir-s3-store get-push-command --id "${ID}" --staging-dir "${STAGING_DIR}"
snapdir-s3-store get-manifest --id "${ID}" --store "${STORE}"
snapdir-s3-store fetch --store "${STORE}" --target-path "${TARGET_PATH}" --store "${STORE}"
snapdir-s3-store get-manifest --id "${ID}" --store "${STORE}"
snapdir-s3-store fetch --store "${STORE}" --target-path "${TARGET_PATH}" --store "${STORE}"
snapdir-s3-store ensure-no-errors --store "${STORE}" "${DIR}"
snapdir push --id "${ID}" --verbose --store "${STORE}"
snapdir-s3-store get-push-command --id "${ID}" --staging-dir "${STAGING_DIR}"
snapdir-s3-store get-manifest --id "${ID}" --store "${STORE}"
snapdir-s3-store fetch --store "${STORE}" --target-path "${TARGET_PATH}" --store "${STORE}"
snapdir-s3-store ensure-no-errors --store "${STORE}" "${DIR}"
snapdir push --id "${ID}" --store "${STORE}"
snapdir-s3-store get-push-command --id "${ID}" --staging-dir "${STAGING_DIR}"
snapdir-s3-store get-manifest --id "${ID}" --store "${STORE}"
snapdir-s3-store fetch --store "${STORE}" --target-path "${TARGET_PATH}" --store "${STORE}"
snapdir-s3-store ensure-no-errors --store "${STORE}" "${DIR}"
snapdir fetch --dryrun --store "${STORE}"
snapdir-s3-store get-manifest-command --id "${ID}" --store "${STORE}"
snapdir-s3-store get-manifest --id "${ID}" --store "${STORE}"
snapdir-s3-store fetch --store "${STORE}" --target-path "${TARGET_PATH}" --store "${STORE}"
snapdir-s3-store ensure-no-errors --store "${STORE}" "${DIR}"
snapdir-s3-store get-fetch-files-command --id "${ID}" --store "${STORE}" "${DIR}"
snapdir fetch --store "${STORE}"
snapdir-s3-store get-manifest-command --id "${ID}" --store "${STORE}"
snapdir-s3-store get-manifest --id "${ID}" --store "${STORE}"
snapdir-s3-store fetch --store "${STORE}" --target-path "${TARGET_PATH}" --store "${STORE}"
snapdir-s3-store ensure-no-errors --store "${STORE}" "${DIR}"
snapdir-s3-store get-fetch-files-command --id "${ID}" --store "${STORE}" "${DIR}"
snapdir-s3-store ensure-no-errors --store "${STORE}" "${DIR}"
snapdir-s3-store fetch --store "${STORE}" --target-path "${TARGET_PATH}" --log-file "${LOG_PATH}"
snapdir-s3-store fetch --store "${STORE}" --target-path "${TARGET_PATH}" --log-file "${LOG_PATH}"
snapdir fetch --store "${STORE}"
snapdir-s3-store get-manifest-command --id bogus --store "${STORE}"
snapdir fetch --dryrun --store "${STORE}"
snapdir pull --dryrun --verbose --store "${STORE}" "${DIR}"
snapdir-s3-store get-fetch-files-command --id "${ID}" --store "${STORE}" "${DIR}"
snapdir pull --verbose --store "${STORE}" "${DIR}"
snapdir-s3-store get-fetch-files-command --id "${ID}" --store "${STORE}" "${DIR}"
snapdir-s3-store ensure-no-errors --store "${STORE}" "${DIR}"