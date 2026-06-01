//! Tests for the hidden `completions <shell>` / `man` build-time hooks the
//! release pipeline (`release.yml` gen-assets job) calls to bundle shell
//! completions + a man page into each release archive. These subcommands are
//! `#[command(hide = true)]`, so they must NOT appear in the documented
//! 14-subcommand surface (`cli_surface`), yet must still parse and generate.

use assert_cmd::Command;

/// `snapdir completions <shell>` exits 0 and emits a non-empty completion
/// script that mentions the binary, for every shell the release pipeline asks
/// for (`bash fish zsh powershell`).
#[test]
fn completions_man_generate_for_every_pipeline_shell() {
    for shell in ["bash", "fish", "zsh", "powershell"] {
        let assert = Command::cargo_bin("snapdir")
            .unwrap()
            .args(["completions", shell])
            .assert()
            .success();
        let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
        assert!(
            !stdout.trim().is_empty(),
            "completions for {shell} were empty"
        );
        assert!(
            stdout.contains("snapdir"),
            "completions for {shell} never mention `snapdir`"
        );
    }
}

/// `snapdir completions bash` looks like a bash completion script: it carries a
/// `complete`/`_snapdir` token the shell sources.
#[test]
fn completions_man_bash_looks_like_a_bash_completion() {
    let assert = Command::cargo_bin("snapdir")
        .unwrap()
        .args(["completions", "bash"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("complete") || stdout.contains("_snapdir"),
        "bash completion missing the expected `complete`/`_snapdir` token"
    );
}

/// `snapdir man` exits 0 and emits non-empty roff: a `.TH` header naming
/// `SNAPDIR`.
#[test]
fn completions_man_renders_roff() {
    let assert = Command::cargo_bin("snapdir")
        .unwrap()
        .arg("man")
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(!stdout.trim().is_empty(), "man page was empty");
    assert!(
        stdout.contains(".TH"),
        "man page missing the .TH roff header"
    );
    // clap_mangen emits the bin name verbatim (lowercase `snapdir`) in the
    // `.TH` title; match SNAPDIR case-insensitively.
    assert!(
        stdout.to_uppercase().contains("SNAPDIR"),
        "man page never names SNAPDIR in its header"
    );
}

/// An unknown shell is rejected (non-zero exit) — the clap value parser only
/// accepts the known `clap_complete::Shell` variants.
#[test]
fn completions_man_unknown_shell_errors() {
    Command::cargo_bin("snapdir")
        .unwrap()
        .args(["completions", "definitely-not-a-shell"])
        .assert()
        .failure();
}
