//! Integration tests for the `snapdir manifest` / `snapdir id` subcommands.
//!
//! These drive the compiled `snapdir` binary over a known tiny scratch tree
//! built with **explicit, fixed permissions** (files `0o644`, dirs `0o755`) so
//! its `TYPE PERMS CHECKSUM SIZE PATH` stdout is fully deterministic, and assert
//! it against **embedded golden constants**. They previously diffed against the
//! frozen Bash differential oracle (now deleted from the branch); the frozen
//! byte-format contract is anchored by
//! `crates/snapdir-core/tests/compat_golden.rs`. What remains here is
//! the CLI-binary wiring coverage: that the `manifest` subcommand honors
//! `--absolute`, `--checksum-bin md5sum|sha256sum`, `--exclude`, and the
//! `SNAPDIR_MANIFEST_CONTEXT` keyed mode, and that `id` emits the snapshot id.
//!
//! Golden values were captured once from this binary over the fixture tree and
//! cross-checked field-by-field against the recorded checksum vectors in
//! `compat_golden.rs` (e.g. blake3("hello")=`ea8f163d…`, blake3("")=`af1349b9…`,
//! md5("")=`d41d8cd9…`, sha256("")=`e3b0c442…`).

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Path to the compiled `snapdir` binary under test.
///
/// The bin target lives in the `snapdir` crate (`crates/snapdir`), so
/// `CARGO_BIN_EXE_snapdir` is not set for snapdir-cli tests; `assert_cmd`'s
/// lookup falls back to the shared target dir. Under `cargo test --workspace`
/// the binary is always built first; for a standalone
/// `cargo test -p snapdir-cli`, run `cargo build -p snapdir` once before.
fn snapdir_bin() -> std::path::PathBuf {
    assert_cmd::cargo::cargo_bin("snapdir")
}

/// Creates a unique temp directory for a test tree and returns its path.
fn temp_tree(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    let unique = format!(
        "snapdir-cli-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    dir.push(unique);
    fs::create_dir_all(&dir).expect("create temp tree");
    dir
}

/// Builds the standard scratch tree with fixed permissions:
///
/// ```text
/// <root>/a.txt        ("hello", 0o644)
/// <root>/empty        ("",      0o644)
/// <root>/sub/         (0o755)
/// <root>/sub/b.txt    ("world!!", 0o644)
/// ```
fn build_basic_tree(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(root.join("a.txt"), b"hello").unwrap();
    fs::write(root.join("empty"), b"").unwrap();
    fs::create_dir(root.join("sub")).unwrap();
    fs::write(root.join("sub").join("b.txt"), b"world!!").unwrap();
    for (rel, mode) in [
        ("a.txt", 0o644),
        ("empty", 0o644),
        ("sub/b.txt", 0o644),
        ("sub", 0o755),
    ] {
        fs::set_permissions(root.join(rel), fs::Permissions::from_mode(mode)).unwrap();
    }
    fs::set_permissions(root, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Runs a command and returns its stdout as a `String`, asserting success.
fn run_stdout(program: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
    let mut cmd = Command::new(program);
    cmd.args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let output = cmd
        .output()
        .unwrap_or_else(|e| panic!("failed to run {}: {e}", program.display()));
    assert!(
        output.status.success(),
        "{} {args:?} exited with {:?}\nstderr: {}",
        program.display(),
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("stdout is UTF-8")
}

/// The default (BLAKE3) golden manifest for the basic tree. Trailing newline
/// included (the `manifest` subcommand emits one).
const GOLDEN_B3: &str = "\
D 755 8aef26c9096d23a66415a42f070250ccbfb616113a299462c44cdc1a21c37d4b 12 ./
F 644 ea8f163db38682925e4491c5e58d4bb3506ef8c14eb78a86e908c5624a67200f 5 ./a.txt
F 644 af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262 0 ./empty
D 755 86659b1ecbc3c43c105315cb5c9c12f376527cc5dcc14a7bb6a8b99676e88b0e 7 ./sub/
F 644 8bafa24d36bc2aa6edc0d041e763cb59ebadb71b6e63ab4ac9314de95e9a0de7 7 ./sub/b.txt
";

#[test]
fn manifest_default_b3_golden() {
    let root = temp_tree("b3");
    build_basic_tree(&root);
    let root_str = root.to_string_lossy().into_owned();

    let actual = run_stdout(&snapdir_bin(), &["manifest", &root_str], &[]);
    assert_eq!(actual, GOLDEN_B3, "default b3 manifest");
    fs::remove_dir_all(&root).ok();
}

#[test]
fn manifest_absolute_golden() {
    // --absolute renders every PATH as the scratch-root prefix + relative tail;
    // all other columns equal the default golden. Reconstruct the expected text
    // by rewriting the relative golden's `./` to the absolute root.
    let root = temp_tree("abs");
    build_basic_tree(&root);
    let root_str = root.to_string_lossy().into_owned();

    let mut expected = String::new();
    for line in GOLDEN_B3.lines() {
        let (head, path) = line.rsplit_once(' ').unwrap();
        let abs = if path == "./" {
            format!("{root_str}/")
        } else {
            format!("{root_str}/{}", path.strip_prefix("./").unwrap())
        };
        writeln!(expected, "{head} {abs}").unwrap();
    }

    let actual = run_stdout(&snapdir_bin(), &["manifest", "--absolute", &root_str], &[]);
    assert_eq!(actual, expected, "--absolute manifest");
    fs::remove_dir_all(&root).ok();
}

#[test]
fn manifest_md5_golden() {
    let root = temp_tree("md5");
    build_basic_tree(&root);
    let root_str = root.to_string_lossy().into_owned();

    let expected = "\
D 755 adb70ffcb744b5b31d681ace791b0a32 12 ./
F 644 5d41402abc4b2a76b9719d911017c592 5 ./a.txt
F 644 d41d8cd98f00b204e9800998ecf8427e 0 ./empty
D 755 4d1b45a28d56adfbb6c561229078cca4 7 ./sub/
F 644 644fde6d4d61626af24f1c5431fa7a97 7 ./sub/b.txt
";
    let actual = run_stdout(
        &snapdir_bin(),
        &["manifest", "--checksum-bin", "md5sum", &root_str],
        &[],
    );
    assert_eq!(actual, expected, "--checksum-bin md5sum manifest");
    fs::remove_dir_all(&root).ok();
}

#[test]
fn manifest_sha256_golden() {
    let root = temp_tree("sha256");
    build_basic_tree(&root);
    let root_str = root.to_string_lossy().into_owned();

    let expected = "\
D 755 61529f2483cf92de7ee91b1ec69a24acfcad2bfc518f9723c66c7d103508711e 12 ./
F 644 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824 5 ./a.txt
F 644 e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 0 ./empty
D 755 9f9e42210f9a39f7b1da7918867e4dd300c8e2cbce6765fc05dac9b332ba54a4 7 ./sub/
F 644 42a19f77459c45b0218f7d21a485a8a41e3bcb4e80e6e570033c49e2fb16446a 7 ./sub/b.txt
";
    let actual = run_stdout(
        &snapdir_bin(),
        &["manifest", "--checksum-bin", "sha256sum", &root_str],
        &[],
    );
    assert_eq!(actual, expected, "--checksum-bin sha256sum manifest");
    fs::remove_dir_all(&root).ok();
}

#[test]
fn manifest_exclude_golden() {
    // --exclude sub drops the ./sub/ subtree; the root D line's checksum/size
    // change accordingly (now only a.txt + empty).
    let root = temp_tree("exclude");
    build_basic_tree(&root);
    let root_str = root.to_string_lossy().into_owned();

    let expected = "\
D 755 d118173369a5e37ee4e7b3e5e6ba59e4a75627b394402441c0d2f4fadbc1cf22 5 ./
F 644 ea8f163db38682925e4491c5e58d4bb3506ef8c14eb78a86e908c5624a67200f 5 ./a.txt
F 644 af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262 0 ./empty
";
    let actual = run_stdout(
        &snapdir_bin(),
        &["manifest", "--exclude", "sub", &root_str],
        &[],
    );
    assert_eq!(actual, expected, "--exclude sub manifest");
    assert!(!actual.contains("sub"), "sub/ excluded");
    fs::remove_dir_all(&root).ok();
}

#[test]
fn manifest_keyed_context_golden() {
    // SNAPDIR_MANIFEST_CONTEXT switches to keyed BLAKE3 (derive_key), changing
    // every content checksum versus the unkeyed default.
    let root = temp_tree("keyed");
    build_basic_tree(&root);
    let root_str = root.to_string_lossy().into_owned();

    let expected = "\
D 755 5ad57fc9f285422e7eaa7b13c07ff25763ce195fd03e1f13f661d06c836dc8d1 12 ./
F 644 a20641105162e1815491cc713459a5d9ecc96aa2f4fffb8076e515f4675be0c1 5 ./a.txt
F 644 1deb4c54b3972346bfb3e009eec9e3f010bcce425fcfd92e0243b893f93cf1d8 0 ./empty
D 755 eeff000bbd1af1c3e4fefe03c4a4ebf8711cb2f7517767453c42913f8eb9670f 7 ./sub/
F 644 8df06c5f5e6dcfcc59412fc9afafd46cbe4fbfbff16793befe474b152a5a993d 7 ./sub/b.txt
";
    let actual = run_stdout(
        &snapdir_bin(),
        &["manifest", &root_str],
        &[("SNAPDIR_MANIFEST_CONTEXT", "sekret")],
    );
    assert_eq!(
        actual, expected,
        "keyed (SNAPDIR_MANIFEST_CONTEXT) manifest"
    );
    assert_ne!(actual, GOLDEN_B3, "keyed output must differ from unkeyed");
    fs::remove_dir_all(&root).ok();
}

#[test]
fn manifest_id_golden() {
    // `id` == BLAKE3 of the comment-stripped manifest text + trailing newline.
    let root = temp_tree("id");
    build_basic_tree(&root);
    let root_str = root.to_string_lossy().into_owned();

    let actual = run_stdout(&snapdir_bin(), &["id", &root_str], &[]);
    assert_eq!(
        actual.trim_end(),
        "e40705a8733449f93f0413549a20797575b2065669e35e5b6cd1084dc81ebeaf",
        "snapshot id"
    );
    assert_eq!(actual.trim_end().len(), 64, "id is 64 hex chars");
    fs::remove_dir_all(&root).ok();
}
