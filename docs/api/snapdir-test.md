# snapdir-test

Test utilities and runner for snapdir commands.

## Usage:

    # runs all tests for snapdir
		  snapdir-test

		  # soucing into an existing script
    . snapdir-test [sourced-test-file]

The snapdir-test script is intended to be sourced by other
scripts to expose the following functions.

- describe: Describes a group of checks.
- check: Using a check will instruct the test runner to expect
         a fail/pass. The test runner with grep for "check " entries
         to guess how many tests are expected to be implemented.
- fail: Fail the test, shows the message and runs the tear down.
- pass: Decrement the number of pending tests as tracked by check.
- run_tests: Runs the tests.
- run_tests_without_teardown: Runs the tests without tear down.

A temporary directory is created for each test run and can be accessed
via _SNAPDIR_TEST_TMP_DIR. This directory is removed when the test
finishes unless tests are run with `run_tests_without_teardown`.


## Options:

    sourced-test-file  When specified, the test file is sourced and
                       the thest suite will take the basename of the
                       file as the name of the test suite.

## Examples:


     # Import test utilities
     # shellcheck disable=SC1091
     . "$(dirname "${BASH_SOURCE[0]}")/snapdir-test" "${BASH_SOURCE[0]}"

     test_suite() {
       local result

       describe "group of checks description"

       check "check a"
       result=$(echo "a" 2>&1 || true)
       test "$result" == "a" || fail "expected '${result}' to be a" && pass

     }

     run_tests
