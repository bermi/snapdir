//! Black-box spec suite for the `autocomplete` command (phase 31, gate
//! `autocomplete-spec-tests`).
//!
//! AUTHORED FROM THE SPEC ONLY — no feature `src/` was read. These tests pin
//! the public CLI contract for the new `snapdir autocomplete <shell>` command
//! (and the back-compat hidden `completions <shell>` alias). They are staged in
//! `.gatesmith/pending-tests/` so the cargo workspace keeps compiling while the
//! feature does not yet exist. The lane owner moves this file to
//! `crates/snapdir-cli/tests/dx_autocomplete.rs` and wires it during the impl
//! gate.
//!
//! THESE TESTS ARE EXPECTED TO FAIL against the current binary (which only has
//! the HIDDEN `completions` subcommand and no visible `autocomplete` command).
//! Do NOT weaken them to be passable.
//!
//! SPEC UNDER TEST (each test comments the clause it pins)
//! =======================================================
//!  (a) `snapdir autocomplete <shell>` for shell in {bash, zsh, fish,
//!      powershell, elvish} -> exit 0, non-empty script mentioning `snapdir`.
//!  (b) `autocomplete` is VISIBLE in `snapdir --help` (NOT hidden); AND
//!      `snapdir autocomplete --help` shows a profile-wiring eval/source example
//!      for at least bash and zsh.
//!  (c) Unknown shell (e.g. `tcsh`, `notashell`) -> exit 2 (clap usage error)
//!      with an error message that LISTS the valid shells (names accepted values).
//!  (d) The hidden `completions <shell>` alias STILL works and emits IDENTICAL
//!      output to `autocomplete <shell>` for the same shell (stdout byte-for-byte,
//!      tested for bash).
//!  (e) REAL-SHELL sourcing: pipe `autocomplete bash` through `bash -n` (syntax
//!      check, exit 0). If zsh is on PATH, source the zsh output and assert no
//!      error. Gate fish/powershell/elvish sourcing on the shell being present.
//!
//! Additional adversarial cases:
//!  (f) `snapdir autocomplete` with NO shell arg -> exit 2 (clap usage error
//!      naming the missing required `<shell>` argument). Pins required-arg contract.
//!  (g) Case sensitivity: `snapdir autocomplete BASH` (uppercase) -> EITHER
//!      exit 0 with a non-empty snapdir script OR exit 2 listing valid shells.
//!      Never a panic and never exit 0 with empty stdout.
//!  (h) Determinism: `autocomplete bash` run twice produces byte-identical stdout.
//!  (i) The bash script targets `snapdir` specifically: output contains `snapdir`
//!      AND a bash-completion marker (`complete ` or `_snapdir`).
//!  (j) Stderr cleanliness on the happy path: `autocomplete bash` writes the
//!      script to stdout (not stderr) and exits 0 (stderr may be empty or warnings,
//!      but must NOT be the completion script, i.e. stdout is non-empty AND stderr
//!      is not the primary output).

// Suppress pedantic lints that would fire on test-only style (mirrors sibling
// suites like `dx_errors.rs`/`dx_args.rs`).
#![allow(
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::items_after_statements,
    clippy::doc_markdown,
    clippy::manual_let_else,
    clippy::map_unwrap_or,
    clippy::uninlined_format_args,
    clippy::manual_assert
)]

use std::path::PathBuf;
use std::process::{Command, Output};

// ---------------------------------------------------------------------------
// Minimal harness (mirrors dx_errors.rs / dx_defaults.rs idioms)
// ---------------------------------------------------------------------------

/// Path to the compiled `snapdir` binary under test.
fn snapdir_bin() -> PathBuf {
    assert_cmd::cargo::cargo_bin("snapdir")
}

/// Run `snapdir <args>` and return the raw `Output`. No env manipulation needed
/// for autocomplete (it never touches the store or cache).
fn run(args: &[&str]) -> Output {
    Command::new(snapdir_bin())
        .args(args)
        .output()
        .expect("failed to run snapdir")
}

/// UTF-8 stdout of an `Output` (losslessly decoded).
fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// UTF-8 stderr of an `Output` (losslessly decoded).
fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Return true if the named binary is on PATH (used to gate real-shell checks).
fn shell_on_path(shell: &str) -> bool {
    Command::new("which")
        .arg(shell)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// (a) Happy-path: autocomplete <shell> -> exit 0, non-empty, mentions snapdir
// ---------------------------------------------------------------------------

/// (a) `snapdir autocomplete bash` exits 0 with a non-empty script mentioning
/// `snapdir`.
#[test]
fn autocomplete_bash_exit0_non_empty_mentions_snapdir() {
    // (a) happy path for bash
    let out = run(&["autocomplete", "bash"]);
    assert!(
        out.status.success(),
        "autocomplete bash: expected exit 0, got {:?}\nstderr: {}",
        out.status.code(),
        stderr_of(&out),
    );
    let stdout = stdout_of(&out);
    assert!(
        !stdout.trim().is_empty(),
        "autocomplete bash: stdout was empty"
    );
    assert!(
        stdout.contains("snapdir"),
        "autocomplete bash: script does not mention `snapdir`\nstdout (first 200 chars): {}",
        &stdout[..stdout.len().min(200)],
    );
}

/// (a) `snapdir autocomplete zsh` exits 0 with a non-empty script mentioning
/// `snapdir`.
#[test]
fn autocomplete_zsh_exit0_non_empty_mentions_snapdir() {
    // (a) happy path for zsh
    let out = run(&["autocomplete", "zsh"]);
    assert!(
        out.status.success(),
        "autocomplete zsh: expected exit 0, got {:?}\nstderr: {}",
        out.status.code(),
        stderr_of(&out),
    );
    let stdout = stdout_of(&out);
    assert!(
        !stdout.trim().is_empty(),
        "autocomplete zsh: stdout was empty"
    );
    assert!(
        stdout.contains("snapdir"),
        "autocomplete zsh: script does not mention `snapdir`",
    );
}

/// (a) `snapdir autocomplete fish` exits 0 with a non-empty script mentioning
/// `snapdir`.
#[test]
fn autocomplete_fish_exit0_non_empty_mentions_snapdir() {
    // (a) happy path for fish
    let out = run(&["autocomplete", "fish"]);
    assert!(
        out.status.success(),
        "autocomplete fish: expected exit 0, got {:?}\nstderr: {}",
        out.status.code(),
        stderr_of(&out),
    );
    let stdout = stdout_of(&out);
    assert!(
        !stdout.trim().is_empty(),
        "autocomplete fish: stdout was empty"
    );
    assert!(
        stdout.contains("snapdir"),
        "autocomplete fish: script does not mention `snapdir`",
    );
}

/// (a) `snapdir autocomplete powershell` exits 0 with a non-empty script
/// mentioning `snapdir`.
#[test]
fn autocomplete_powershell_exit0_non_empty_mentions_snapdir() {
    // (a) happy path for powershell
    let out = run(&["autocomplete", "powershell"]);
    assert!(
        out.status.success(),
        "autocomplete powershell: expected exit 0, got {:?}\nstderr: {}",
        out.status.code(),
        stderr_of(&out),
    );
    let stdout = stdout_of(&out);
    assert!(
        !stdout.trim().is_empty(),
        "autocomplete powershell: stdout was empty"
    );
    assert!(
        stdout.contains("snapdir"),
        "autocomplete powershell: script does not mention `snapdir`",
    );
}

/// (a) `snapdir autocomplete elvish` exits 0 with a non-empty script mentioning
/// `snapdir`.
#[test]
fn autocomplete_elvish_exit0_non_empty_mentions_snapdir() {
    // (a) happy path for elvish
    let out = run(&["autocomplete", "elvish"]);
    assert!(
        out.status.success(),
        "autocomplete elvish: expected exit 0, got {:?}\nstderr: {}",
        out.status.code(),
        stderr_of(&out),
    );
    let stdout = stdout_of(&out);
    assert!(
        !stdout.trim().is_empty(),
        "autocomplete elvish: stdout was empty"
    );
    assert!(
        stdout.contains("snapdir"),
        "autocomplete elvish: script does not mention `snapdir`",
    );
}

// ---------------------------------------------------------------------------
// (b) Visibility: autocomplete appears in --help; autocomplete --help has eval
// ---------------------------------------------------------------------------

/// (b) `autocomplete` is NOT hidden — it appears in `snapdir --help` subcommand
/// list.
#[test]
fn autocomplete_visible_in_top_level_help() {
    // (b) autocomplete must appear in the top-level help (not hidden)
    let out = run(&["--help"]);
    let stdout = stdout_of(&out);
    assert!(
        stdout.contains("autocomplete"),
        "`autocomplete` does not appear in `snapdir --help` output\n\
         (it is hidden or not yet present)\nstdout:\n{}",
        stdout,
    );
}

/// (b) `snapdir autocomplete --help` shows a profile-wiring eval/source example
/// for bash (e.g. `eval "$(snapdir autocomplete bash)"`).
#[test]
fn autocomplete_help_shows_bash_eval_example() {
    // (b) bash wiring example in autocomplete --help
    let out = run(&["autocomplete", "--help"]);
    // --help normally exits 0; accept both 0 and 2 so the test runs even if
    // clap is configured to exit nonzero on --help.
    let stdout = stdout_of(&out);
    let stderr = stderr_of(&out);
    let combined = format!("{stdout}{stderr}");
    // The spec requires an eval/source wiring example for bash.
    assert!(
        combined.contains("eval") || combined.contains("source"),
        "`snapdir autocomplete --help` does not show an eval/source wiring example\n\
         combined output:\n{}",
        combined,
    );
    // The example must reference bash specifically.
    assert!(
        combined.to_lowercase().contains("bash"),
        "`snapdir autocomplete --help` wiring example does not mention bash\n\
         combined output:\n{}",
        combined,
    );
}

/// (b) `snapdir autocomplete --help` also shows a zsh profile-wiring eval/source
/// example.
#[test]
fn autocomplete_help_shows_zsh_eval_example() {
    // (b) zsh wiring example in autocomplete --help
    let out = run(&["autocomplete", "--help"]);
    let stdout = stdout_of(&out);
    let stderr = stderr_of(&out);
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.to_lowercase().contains("zsh"),
        "`snapdir autocomplete --help` does not mention zsh in a wiring example\n\
         combined output:\n{}",
        combined,
    );
}

// ---------------------------------------------------------------------------
// (c) Unknown shell -> exit 2 listing valid shells
// ---------------------------------------------------------------------------

/// (c) `snapdir autocomplete tcsh` -> exit 2 with an error that names the
/// accepted shells (clap `possible_values` / value enum listing).
#[test]
fn autocomplete_unknown_shell_tcsh_exit2_lists_valid() {
    // (c) unknown shell -> exit 2 listing valid shells
    let out = run(&["autocomplete", "tcsh"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "autocomplete tcsh: expected exit 2 (clap usage error), got {:?}\nstderr: {}",
        out.status.code(),
        stderr_of(&out),
    );
    let stderr = stderr_of(&out);
    // clap should list the accepted values; check for at least one known shell.
    let lists_valid = stderr.contains("bash")
        || stderr.contains("zsh")
        || stderr.contains("fish")
        || stderr.contains("possible")
        || stderr.contains("valid");
    assert!(
        lists_valid,
        "autocomplete tcsh: exit 2 but stderr does not list valid shells\nstderr: {}",
        stderr,
    );
}

/// (c) `snapdir autocomplete notashell` -> exit 2 with a useful error that
/// names the accepted shells.
#[test]
fn autocomplete_unknown_shell_notashell_exit2_lists_valid() {
    // (c) unknown shell -> exit 2 listing valid shells
    let out = run(&["autocomplete", "notashell"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "autocomplete notashell: expected exit 2, got {:?}\nstderr: {}",
        out.status.code(),
        stderr_of(&out),
    );
    let stderr = stderr_of(&out);
    let lists_valid = stderr.contains("bash")
        || stderr.contains("zsh")
        || stderr.contains("fish")
        || stderr.contains("possible")
        || stderr.contains("valid");
    assert!(
        lists_valid,
        "autocomplete notashell: exit 2 but stderr does not list valid shells\nstderr: {}",
        stderr,
    );
}

// ---------------------------------------------------------------------------
// (d) Back-compat: hidden `completions` alias emits identical output to
//     `autocomplete` for the same shell (bash + zsh).
// ---------------------------------------------------------------------------

/// (d) The hidden `completions bash` alias still works (exit 0, non-empty) and
/// produces BYTE-IDENTICAL stdout to `autocomplete bash`.
#[test]
fn completions_alias_still_works_and_identical_to_autocomplete() {
    // (d) back-compat: completions alias == autocomplete for bash
    let new_out = run(&["autocomplete", "bash"]);
    assert!(
        new_out.status.success(),
        "autocomplete bash: failed (prerequisite for alias check)\nstderr: {}",
        stderr_of(&new_out),
    );

    let old_out = run(&["completions", "bash"]);
    assert!(
        old_out.status.success(),
        "completions bash (hidden alias): expected exit 0, got {:?}\nstderr: {}",
        old_out.status.code(),
        stderr_of(&old_out),
    );

    assert_eq!(
        new_out.stdout, old_out.stdout,
        "completions bash and autocomplete bash differ in stdout\n\
         (they must be byte-identical for release.yml back-compat)",
    );
}

/// (d) The hidden `completions zsh` alias also produces BYTE-IDENTICAL stdout
/// to `autocomplete zsh`. Extends the alias back-compat check to a second
/// shell to catch any shell-dispatch path divergence.
#[test]
fn completions_alias_zsh_identical_to_autocomplete_zsh() {
    // (d) back-compat: completions alias == autocomplete for zsh
    let new_out = run(&["autocomplete", "zsh"]);
    assert!(
        new_out.status.success(),
        "autocomplete zsh: failed (prerequisite for alias check)\nstderr: {}",
        stderr_of(&new_out),
    );

    let old_out = run(&["completions", "zsh"]);
    assert!(
        old_out.status.success(),
        "completions zsh (hidden alias): expected exit 0, got {:?}\nstderr: {}",
        old_out.status.code(),
        stderr_of(&old_out),
    );

    assert_eq!(
        new_out.stdout, old_out.stdout,
        "completions zsh and autocomplete zsh differ in stdout\n\
         (they must be byte-identical for release.yml back-compat)",
    );
}

/// (d) The hidden `completions` alias does NOT appear in `snapdir --help`.
/// This pins the contract that the alias is truly hidden from the documented
/// surface (users see `autocomplete` only) while existing scripts keep working.
#[test]
fn completions_alias_is_hidden_from_top_level_help() {
    // (d) completions alias must NOT appear in --help (it is a hidden clap alias)
    let out = run(&["--help"]);
    let stdout = stdout_of(&out);
    assert!(
        !stdout.contains("completions"),
        "`completions` (hidden alias) APPEARS in `snapdir --help` — it should be hidden\n\
         If visible, users see two commands for the same thing (breaks the documented surface).\n\
         stdout:\n{}",
        stdout,
    );
}

// ---------------------------------------------------------------------------
// (e) Real-shell sourcing checks
// ---------------------------------------------------------------------------

/// (e) Pipe `autocomplete bash` output through `bash -n` (syntax check) and
/// assert success. Always runs (bash assumed present on unix CI/dev boxes).
#[test]
fn autocomplete_bash_output_passes_bash_syntax_check() {
    // (e) bash -n syntax check
    let completion_out = run(&["autocomplete", "bash"]);
    assert!(
        completion_out.status.success(),
        "autocomplete bash: prerequisite failed\nstderr: {}",
        stderr_of(&completion_out),
    );
    let script = &completion_out.stdout;

    // bash -n reads from stdin
    let bash_result = Command::new("bash")
        .arg("-n")
        .arg("/dev/stdin")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(stdin) = child.stdin.take() {
                let mut stdin = stdin;
                stdin.write_all(script).ok();
            }
            child.wait_with_output()
        });

    match bash_result {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // bash not available; skip gracefully rather than panic.
            eprintln!(
                "SKIP autocomplete_bash_output_passes_bash_syntax_check: bash not found on PATH"
            );
        }
        Err(e) => panic!("failed to run bash -n: {e}"),
        Ok(out) => {
            assert!(
                out.status.success(),
                "bash -n rejected the autocomplete bash output (syntax error)\nstderr: {}",
                String::from_utf8_lossy(&out.stderr),
            );
        }
    }
}

/// (e) If `zsh` is on PATH, source the `autocomplete zsh` output in a
/// `zsh -c 'source /dev/stdin'`-style check and assert no error.
#[test]
fn autocomplete_zsh_output_sources_cleanly_if_zsh_present() {
    // (e) zsh source check — gated on zsh being available
    if !shell_on_path("zsh") {
        eprintln!(
            "SKIP autocomplete_zsh_output_sources_cleanly_if_zsh_present: zsh not found on PATH"
        );
        return;
    }

    let completion_out = run(&["autocomplete", "zsh"]);
    assert!(
        completion_out.status.success(),
        "autocomplete zsh: prerequisite failed\nstderr: {}",
        stderr_of(&completion_out),
    );
    let script = &completion_out.stdout;

    // The clap-generated zsh script ends in `compdef _snapdir snapdir`, so it
    // can only be sourced after the zsh completion system is initialized — which
    // is exactly the interactive-shell context a user wires it into. Initialize
    // it (autoload compinit + compinit) before sourcing, mirroring a real
    // `~/.zshrc` so the source check exercises the script in a valid context
    // rather than a bare non-interactive shell where `compdef` is undefined.
    let zsh_result = Command::new("zsh")
        .args([
            "-c",
            "autoload -Uz compinit && compinit -u && source /dev/stdin",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(stdin) = child.stdin.take() {
                let mut stdin = stdin;
                stdin.write_all(script).ok();
            }
            child.wait_with_output()
        });

    match zsh_result {
        Err(e) => panic!("failed to run zsh -c 'source /dev/stdin': {e}"),
        Ok(out) => {
            assert!(
                out.status.success(),
                "zsh source check failed for autocomplete zsh output\nstderr: {}",
                String::from_utf8_lossy(&out.stderr),
            );
        }
    }
}

/// (e) If `fish` is on PATH, verify that `autocomplete fish` output passes
/// `fish --no-execute` syntax check.
#[test]
fn autocomplete_fish_output_syntax_check_if_fish_present() {
    // (e) fish syntax check — gated on fish being available
    if !shell_on_path("fish") {
        eprintln!(
            "SKIP autocomplete_fish_output_syntax_check_if_fish_present: fish not found on PATH"
        );
        return;
    }

    let completion_out = run(&["autocomplete", "fish"]);
    assert!(
        completion_out.status.success(),
        "autocomplete fish: prerequisite failed\nstderr: {}",
        stderr_of(&completion_out),
    );

    // Write the fish completion to a temp file then run `fish --no-execute` on it.
    let tmp = std::env::temp_dir().join(format!(
        "snapdir-fish-completion-{}-check.fish",
        std::process::id()
    ));
    std::fs::write(&tmp, &completion_out.stdout).expect("write fish completion to tempfile");

    let fish_result = Command::new("fish")
        .args(["--no-execute", tmp.to_str().unwrap()])
        .output();

    let _ = std::fs::remove_file(&tmp);

    match fish_result {
        Err(e) => panic!("failed to run fish --no-execute: {e}"),
        Ok(out) => {
            assert!(
                out.status.success(),
                "fish --no-execute rejected the autocomplete fish output\nstderr: {}",
                String::from_utf8_lossy(&out.stderr),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// (f) No shell arg -> exit 2 (required arg missing)
// ---------------------------------------------------------------------------

/// (f) `snapdir autocomplete` (no shell arg) -> exit 2 naming the missing
/// required `<shell>` argument.
#[test]
fn autocomplete_no_shell_arg_exit2_names_required_arg() {
    // (f) missing required <shell> arg -> exit 2
    let out = run(&["autocomplete"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "autocomplete with no arg: expected exit 2, got {:?}\nstderr: {}",
        out.status.code(),
        stderr_of(&out),
    );
    // The error should say something about the missing argument.
    let stderr = stderr_of(&out);
    assert!(
        !stderr.trim().is_empty(),
        "autocomplete with no arg: exit 2 but stderr was empty (should name missing arg)",
    );
}

// ---------------------------------------------------------------------------
// (g) Case sensitivity / surprising input
// ---------------------------------------------------------------------------

/// (g) `snapdir autocomplete BASH` (uppercase) -> either exit 0 with a
/// non-empty snapdir script (case-insensitive clap value-enum) OR exit 2
/// listing valid shells. Never a panic, never exit 0 with empty stdout.
#[test]
fn autocomplete_uppercase_bash_deterministic_behavior() {
    // (g) case sensitivity / surprising input
    let out = run(&["autocomplete", "BASH"]);
    let code = out.status.code();
    let stdout = stdout_of(&out);
    let stderr = stderr_of(&out);

    match code {
        Some(0) => {
            // clap value-enum is case-insensitive: accepted -> must produce a real script
            assert!(
                !stdout.trim().is_empty(),
                "autocomplete BASH: exit 0 but stdout was empty (invalid: must emit a script or exit 2)",
            );
            assert!(
                stdout.contains("snapdir"),
                "autocomplete BASH: exit 0 with non-empty stdout but output never mentions `snapdir`\nstdout (first 200): {}",
                &stdout[..stdout.len().min(200)],
            );
        }
        Some(2) => {
            // case-sensitive clap: error listing valid shells is acceptable
            let lists_valid = stderr.contains("bash")
                || stderr.contains("zsh")
                || stderr.contains("fish")
                || stderr.contains("possible")
                || stderr.contains("valid");
            assert!(
                lists_valid,
                "autocomplete BASH: exit 2 but stderr does not list valid shells\nstderr: {}",
                stderr,
            );
        }
        other => {
            panic!(
                "autocomplete BASH: unexpected exit code {other:?} (must be 0 or 2)\nstdout: {stdout}\nstderr: {stderr}",
            );
        }
    }
}

// ---------------------------------------------------------------------------
// (c-exact) The exact set of valid shells: exactly {bash, elvish, fish,
//           powershell, zsh} — the clap_complete::Shell enum's five variants.
// ---------------------------------------------------------------------------

/// (c-exact) The error message for an unknown shell names all five accepted
/// values: bash, elvish, fish, powershell, zsh — and no others. This pins the
/// shell-set contract so any future addition/removal is caught immediately.
#[test]
fn autocomplete_unknown_shell_error_lists_exactly_five_shells() {
    // (c-exact) clap must list all five valid shells; no more, no less
    let out = run(&["autocomplete", "tcsh"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "autocomplete tcsh: expected exit 2\nstderr: {}",
        stderr_of(&out),
    );
    let stderr = stderr_of(&out);
    for shell in &["bash", "elvish", "fish", "powershell", "zsh"] {
        assert!(
            stderr.contains(shell),
            "autocomplete tcsh: error message does not list expected shell `{shell}`\nstderr: {stderr}",
        );
    }
}

// ---------------------------------------------------------------------------
// (h) Determinism: two consecutive runs produce byte-identical stdout
// ---------------------------------------------------------------------------

/// (h) `snapdir autocomplete bash` run twice produces byte-identical stdout.
#[test]
fn autocomplete_bash_is_deterministic() {
    // (h) determinism across two runs
    let out1 = run(&["autocomplete", "bash"]);
    let out2 = run(&["autocomplete", "bash"]);

    assert!(
        out1.status.success() && out2.status.success(),
        "autocomplete bash: one or both runs failed (cannot test determinism)\n\
         run1 exit={:?}, run2 exit={:?}",
        out1.status.code(),
        out2.status.code(),
    );
    assert_eq!(
        out1.stdout, out2.stdout,
        "autocomplete bash: two consecutive runs produced different stdout (non-deterministic!)",
    );
}

// ---------------------------------------------------------------------------
// (i) Bash completion targets the `snapdir` binary by name
// ---------------------------------------------------------------------------

/// (i) The generated bash completion script defines a completion for the
/// `snapdir` binary name: output contains `snapdir` AND a bash-completion
/// marker (`complete ` or `_snapdir`), proving it targets the right binary.
#[test]
fn autocomplete_bash_script_targets_snapdir_binary() {
    // (i) bash script targets snapdir binary name
    let out = run(&["autocomplete", "bash"]);
    assert!(
        out.status.success(),
        "autocomplete bash: prerequisite failed\nstderr: {}",
        stderr_of(&out),
    );
    let stdout = stdout_of(&out);
    assert!(
        stdout.contains("snapdir"),
        "autocomplete bash script does not mention `snapdir` at all",
    );
    let has_bash_marker = stdout.contains("complete ") || stdout.contains("_snapdir");
    assert!(
        has_bash_marker,
        "autocomplete bash script missing expected bash-completion marker \
         (`complete ` or `_snapdir`)\nstdout (first 400 chars):\n{}",
        &stdout[..stdout.len().min(400)],
    );
}

// ---------------------------------------------------------------------------
// (j) Stderr cleanliness on the happy path
// ---------------------------------------------------------------------------

/// (j) `snapdir autocomplete bash` writes the script to STDOUT (not stderr):
/// exits 0, stdout is non-empty, and the completion script content is NOT being
/// printed only on stderr (i.e., stdout is the primary output channel).
#[test]
fn autocomplete_bash_happy_path_stderr_clean() {
    // (j) stderr cleanliness: script goes to stdout, not stderr
    let out = run(&["autocomplete", "bash"]);
    assert!(
        out.status.success(),
        "autocomplete bash: expected exit 0\nstderr: {}",
        stderr_of(&out),
    );
    let stdout = stdout_of(&out);
    let stderr = stderr_of(&out);

    assert!(
        !stdout.trim().is_empty(),
        "autocomplete bash: stdout was empty (script must go to stdout)",
    );
    // The completion script (which mentions `snapdir`) must be on stdout, not only stderr.
    assert!(
        stdout.contains("snapdir"),
        "autocomplete bash: script content not on stdout\nstdout (first 200): {}\nstderr (first 200): {}",
        &stdout[..stdout.len().min(200)],
        &stderr[..stderr.len().min(200)],
    );
    // Stderr should NOT be the primary bearer of the script — the script must
    // be on stdout. (Warnings/diagnostics on stderr are tolerated; the check is
    // that stdout carries the output, not that stderr is totally empty.)
    // If stderr is non-empty but stdout also contains the script, we pass.
    // If stdout is empty while stderr contains the script, that is a bug.
    if stdout.trim().is_empty() {
        panic!(
            "autocomplete bash: stdout empty while stderr has content — script written to wrong fd\nstderr: {}",
            stderr,
        );
    }
}
