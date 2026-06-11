//! Gate tests for the hidden SNAPPACK plumbing subcommands
//! (`version --capabilities`, `objects-needed`, `send-pack`, `receive-pack`)
//! that power the upcoming `ssh://` store acceleration
//! (`<local> snapdir send-pack | ssh host 'snapdir receive-pack'`).
//!
//! Everything here drives the REAL binary (resolved via `snapdir_bin()`)
//! against temp `file://` stores, mirroring how the existing e2e tests build
//! fixtures: snapshot fixtures are pushed with the binary itself; raw object
//! fixtures are seeded via the `snapdir-stores` `FileStore`/`StreamStore`
//! helpers (regular dependencies, available to integration tests). Hermetic —
//! no network, no credentials, every temp dir removed on drop.

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use assert_fs::prelude::*;
use assert_fs::TempDir;
use snapdir_core::{manifest_path, object_path, Blake3Hasher, Hasher, Store};
use snapdir_stores::{FileStore, StreamStore, WIRE_CAPS, WIRE_VERSION};

/// The real binary under test.
///
/// The bin target lives in the `snapdir` crate (`crates/snapdir`), so
/// `CARGO_BIN_EXE_snapdir` is not set for snapdir-cli tests; `assert_cmd`'s
/// lookup falls back to the shared target dir. Under `cargo test --workspace`
/// the binary is always built first; for a standalone
/// `cargo test -p snapdir-cli`, run `cargo build -p snapdir` once before.
fn snapdir_bin() -> std::path::PathBuf {
    assert_cmd::cargo::cargo_bin("snapdir")
}

/// A fresh `snapdir` command with the cache pinned under `cache` and the
/// store-selecting env cleared, so a leaked `SNAPDIR_STORE` (clap `env` flag)
/// can never perturb a test.
fn snapdir(cache: &Path) -> Command {
    let mut cmd = Command::new(snapdir_bin());
    cmd.env("SNAPDIR_CACHE_DIR", cache);
    cmd.env_remove("SNAPDIR_STORE");
    cmd
}

/// Runs `snapdir <args>` with `stdin_bytes` piped in, returning the full
/// `Output` (the caller asserts status/stdout/stderr).
fn run_with_stdin(cache: &Path, args: &[&str], stdin_bytes: &[u8]) -> Output {
    let mut child = snapdir(cache)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn snapdir");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(stdin_bytes)
        .expect("write stdin");
    child.wait_with_output().expect("snapdir output")
}

/// Runs `snapdir <args>`, asserts success, returns trimmed stdout (the same
/// helper shape e2e.rs uses).
fn stdout_ok(cache: &Path, args: &[&str]) -> String {
    let out = snapdir(cache).args(args).output().expect("run snapdir");
    assert!(
        out.status.success(),
        "snapdir {args:?} failed ({:?})\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8(out.stdout).unwrap().trim_end().to_owned()
}

/// Builds a small deterministic tree (multiple distinct objects + explicit
/// perms) for binary-driven push fixtures, like e2e.rs's `build_tree`.
fn build_tree(dir: &TempDir) {
    dir.child("a.txt").write_str("plumbing alpha").unwrap();
    dir.child("sub/b.txt")
        .write_str("plumbing bravo!!")
        .unwrap();
    dir.child("sub/c.bin")
        .write_str("plumbing charlie payload")
        .unwrap();
    for (rel, mode) in [("a.txt", 0o644), ("sub/b.txt", 0o600), ("sub/c.bin", 0o644)] {
        std::fs::set_permissions(dir.child(rel).path(), PermissionsExt::from_mode(mode)).unwrap();
    }
    std::fs::set_permissions(dir.child("sub").path(), PermissionsExt::from_mode(0o755)).unwrap();
    std::fs::set_permissions(dir.path(), PermissionsExt::from_mode(0o755)).unwrap();
}

/// Pushes `build_tree` into a fresh `file://` store via the binary, returning
/// `(store_dir, snapshot_id, object_ids)` — the object ids come from the
/// stored manifest's File entries (deduped, manifest order).
fn pushed_fixture(cache: &Path) -> (TempDir, String, Vec<String>) {
    let src = TempDir::new().unwrap();
    let store = TempDir::new().unwrap();
    build_tree(&src);
    let src_str = src.path().to_string_lossy().into_owned();
    let store_url = format!("file://{}", store.path().display());
    let id = stdout_ok(cache, &["push", "--store", &store_url, &src_str]);

    let manifest = FileStore::from_root(store.path())
        .get_manifest(&id)
        .expect("manifest in pushed store");
    let mut object_ids: Vec<String> = Vec::new();
    for entry in manifest.entries() {
        if entry.path_type == snapdir_core::PathType::File && !object_ids.contains(&entry.checksum)
        {
            object_ids.push(entry.checksum.clone());
        }
    }
    assert!(object_ids.len() >= 3, "fixture must carry several objects");
    (store, id, object_ids)
}

/// BLAKE3 of `bytes` as the 64-hex content address.
fn hex_of(bytes: &[u8]) -> String {
    Blake3Hasher::new().hash_hex(bytes)
}

/// Recursively collects every regular file under `dir` (empty if absent) —
/// used to prove a rejected stream filed nothing (not even temp litter).
fn files_under(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return files;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(files_under(&path));
        } else {
            files.push(path);
        }
    }
    files
}

// ---------------------------------------------------------------------------
// version --capabilities
// ---------------------------------------------------------------------------

/// The capability line is EXACTLY `snapdir <semver> wire=1
/// caps=objects-needed,send-pack,receive-pack,snappack-zstd\n` — pinned both to
/// the literal wire-1 grammar the remote probe matches AND to the lib constants
/// it must bake in (so a constant bump can't silently desync the CLI). The
/// `snappack-zstd` token is additive: the wire integer stays `1`, so older peers
/// (whose `_snapdir_caps_ok` ignores unknown tokens) keep negotiating cleanly.
#[test]
fn plumbing_capabilities_line_exact() {
    let cache = TempDir::new().unwrap();
    let out = snapdir(cache.path())
        .args(["version", "--capabilities"])
        .output()
        .expect("run snapdir");
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    // Literal wire-1 pin (the probe negotiates on this exact integer); the
    // `snappack-zstd` token is present and last.
    assert_eq!(
        stdout,
        format!(
            "snapdir {} wire=1 caps=objects-needed,send-pack,receive-pack,snappack-zstd\n",
            env!("CARGO_PKG_VERSION")
        )
    );
    // The new zstd capability MUST be advertised (and via the lib const, never a
    // hand-written literal that could drift from `WIRE_CAPS`).
    assert!(
        stdout.contains("snappack-zstd"),
        "version --capabilities must advertise snappack-zstd: {stdout:?}"
    );
    // And the same line re-derived from the lib constants.
    assert_eq!(
        stdout,
        format!(
            "snapdir {} wire={WIRE_VERSION} caps={}\n",
            env!("CARGO_PKG_VERSION"),
            WIRE_CAPS.join(",")
        )
    );
}

/// Plain `snapdir version` stays byte-identical to the frozen surface.
#[test]
fn plumbing_plain_version_unchanged() {
    let cache = TempDir::new().unwrap();
    let out = snapdir(cache.path())
        .arg("version")
        .output()
        .expect("run snapdir");
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!("snapdir {}\n", env!("CARGO_PKG_VERSION"))
    );
}

// ---------------------------------------------------------------------------
// objects-needed
// ---------------------------------------------------------------------------

/// Seed a store with a SUBSET of objects, then offer the full ordered list
/// (with duplicates) on stdin: stdout must be the EXACT complement, deduped,
/// in first-occurrence order.
#[test]
fn plumbing_objects_needed_prints_exact_complement_in_order() {
    let cache = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store = FileStore::from_root(store_dir.path());

    // Present: two seeded objects. Absent: two valid-but-unseeded addresses.
    let present: Vec<String> = [b"seeded one".as_slice(), b"seeded two".as_slice()]
        .iter()
        .map(|payload| {
            let checksum = hex_of(payload);
            store
                .put_object(&checksum, payload.to_vec())
                .expect("seed object");
            checksum
        })
        .collect();
    let absent_a = hex_of(b"absent a");
    let absent_b = hex_of(b"absent b");

    // Full ordered list with duplicates of both kinds sprinkled in.
    let stdin = format!(
        "{p0}\n{a}\n{p0}\n{p1}\n{b}\n{a}\n{p1}\n",
        p0 = present[0],
        p1 = present[1],
        a = absent_a,
        b = absent_b,
    );
    let store_url = format!("file://{}", store_dir.path().display());
    let out = run_with_stdin(
        cache.path(),
        &["objects-needed", "--store", &store_url],
        stdin.as_bytes(),
    );
    assert!(
        out.status.success(),
        "objects-needed failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!("{absent_a}\n{absent_b}\n"),
        "stdout must be the exact absent complement in first-occurrence order"
    );
}

/// ANY malformed line (uppercase / 63-char / non-hex) fails the WHOLE request
/// with a non-zero exit and an EMPTY stdout — even when valid absent ids
/// precede it (fail closed: validation happens before the first store query).
#[test]
fn plumbing_objects_needed_malformed_line_fails_closed() {
    let cache = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store_url = format!("file://{}", store_dir.path().display());
    let valid_absent = hex_of(b"valid but absent");
    let hex = "0123456789abcdef".repeat(4);

    for bad in [
        hex.to_uppercase(),        // uppercase
        hex[..63].to_owned(),      // 63 chars
        format!("g{}", &hex[1..]), // non-hex char
    ] {
        let stdin = format!("{valid_absent}\n{bad}\n");
        let out = run_with_stdin(
            cache.path(),
            &["objects-needed", "--store", &store_url],
            stdin.as_bytes(),
        );
        assert!(
            !out.status.success(),
            "malformed line {bad:?} must fail the request"
        );
        assert!(
            out.stdout.is_empty(),
            "malformed line {bad:?} must print NOTHING to stdout, got: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

/// Empty stdin is a valid (empty) request: exit 0, empty stdout.
#[test]
fn plumbing_objects_needed_empty_input_is_ok() {
    let cache = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store_url = format!("file://{}", store_dir.path().display());
    let out = run_with_stdin(
        cache.path(),
        &["objects-needed", "--store", &store_url],
        b"",
    );
    assert!(out.status.success(), "empty input must succeed");
    assert!(out.stdout.is_empty(), "empty input must print nothing");
}

// ---------------------------------------------------------------------------
// send-pack | receive-pack round trip (real OS pipe between the two binaries)
// ---------------------------------------------------------------------------

/// `send-pack --store file://A --ids - --manifest-id <id>` piped into
/// `receive-pack --store file://B --require-manifest <id>`: B ends up with the
/// manifest + byte-equal objects at the identical sharded on-disk paths, and
/// receive-pack's stdout stays silent.
#[test]
fn plumbing_pack_roundtrip_via_pipe() {
    let cache = TempDir::new().unwrap();
    let (store_a, id, object_ids) = pushed_fixture(cache.path());
    let store_b = TempDir::new().unwrap();
    let url_a = format!("file://{}", store_a.path().display());
    let url_b = format!("file://{}", store_b.path().display());

    // Spawn the sender, feed the id list on ITS stdin, and connect its stdout
    // to the receiver's stdin via a real OS pipe (process plumbing — exactly
    // the future `| ssh host 'snapdir receive-pack'` shape).
    let mut send = snapdir(cache.path())
        .args([
            "send-pack",
            "--store",
            &url_a,
            "--ids",
            "-",
            "--manifest-id",
            &id,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn send-pack");
    send.stdin
        .take()
        .expect("send-pack stdin")
        .write_all(format!("{}\n", object_ids.join("\n")).as_bytes())
        .expect("write id list");
    let pack_pipe = send.stdout.take().expect("send-pack stdout");

    let recv = snapdir(cache.path())
        .args(["receive-pack", "--store", &url_b, "--require-manifest", &id])
        .stdin(Stdio::from(pack_pipe))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run receive-pack");
    let send_out = send.wait_with_output().expect("send-pack exit");

    assert!(
        send_out.status.success(),
        "send-pack failed: {}",
        String::from_utf8_lossy(&send_out.stderr)
    );
    assert!(
        recv.status.success(),
        "receive-pack failed: {}",
        String::from_utf8_lossy(&recv.stderr)
    );
    assert!(
        recv.stdout.is_empty(),
        "receive-pack must print nothing to stdout"
    );

    // B holds the manifest at its sharded path, byte-equal to A's...
    let man_rel = manifest_path(&id);
    assert_eq!(
        std::fs::read(store_b.path().join(&man_rel)).expect("manifest in B"),
        std::fs::read(store_a.path().join(&man_rel)).expect("manifest in A"),
        "manifest must be byte-equal at the identical sharded path"
    );
    // ...and every object, byte-equal at the identical sharded paths.
    for checksum in &object_ids {
        let rel = object_path(checksum);
        assert_eq!(
            std::fs::read(store_b.path().join(&rel)).expect("object in B"),
            std::fs::read(store_a.path().join(&rel)).expect("object in A"),
            "object {rel} must be byte-equal"
        );
    }
}

/// Spawns `send-pack --store file://A --ids - --manifest-id <id>` against the
/// fixture store, feeding the object-id list on its stdin and appending any
/// `extra` flags (e.g. `--pack-format zstd`), and returns the raw pack bytes it
/// emitted to stdout (after asserting the command succeeded). The fixture lives
/// in the caller-owned `store_a`/`id`/`object_ids`.
fn run_send_pack_bytes(
    cache: &Path,
    url_a: &str,
    id: &str,
    object_ids: &[String],
    extra: &[&str],
) -> Vec<u8> {
    let mut args = vec![
        "send-pack",
        "--store",
        url_a,
        "--ids",
        "-",
        "--manifest-id",
        id,
    ];
    args.extend_from_slice(extra);
    let mut send = snapdir(cache)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn send-pack");
    send.stdin
        .take()
        .expect("send-pack stdin")
        .write_all(format!("{}\n", object_ids.join("\n")).as_bytes())
        .expect("write id list");
    let out = send.wait_with_output().expect("send-pack exit");
    assert!(
        out.status.success(),
        "send-pack {extra:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// The hidden `--pack-format zstd` flag emits the additive `SNAPPACK 1Z` magic
/// (distinct from the default v1 `SNAPPACK 1` magic), and that zstd stream
/// `receive-pack`s into a store that is BYTE-IDENTICAL to the v1 roundtrip — the
/// receiver sniffs the magic and needs no matching flag.
#[test]
fn plumbing_send_pack_zstd_roundtrip_is_byte_identical_to_v1() {
    let cache = TempDir::new().unwrap();
    let (store_a, id, object_ids) = pushed_fixture(cache.path());
    let url_a = format!("file://{}", store_a.path().display());

    // The default (no flag) and the explicit `--pack-format v1` both open with
    // the plain v1 magic; the zstd form opens with the 1Z magic.
    let v1_bytes = run_send_pack_bytes(cache.path(), &url_a, &id, &object_ids, &[]);
    let v1_explicit = run_send_pack_bytes(
        cache.path(),
        &url_a,
        &id,
        &object_ids,
        &["--pack-format", "v1"],
    );
    let zstd_bytes = run_send_pack_bytes(
        cache.path(),
        &url_a,
        &id,
        &object_ids,
        &["--pack-format", "zstd"],
    );
    assert!(
        v1_bytes.starts_with(b"SNAPPACK 1\n"),
        "default send-pack must emit the plain v1 magic"
    );
    assert_eq!(
        v1_explicit, v1_bytes,
        "--pack-format v1 must be byte-identical to the default (no flag)"
    );
    assert!(
        zstd_bytes.starts_with(b"SNAPPACK 1Z\n"),
        "--pack-format zstd must emit the additive 1Z magic"
    );
    assert_ne!(
        zstd_bytes, v1_bytes,
        "the zstd stream must differ on the wire from the v1 stream"
    );

    // Receive the zstd stream into a fresh store and compare it object-for-object
    // and manifest-for-manifest against the v1 source store: the decoded result
    // must be byte-identical despite the different transport encoding.
    let store_z = TempDir::new().unwrap();
    let url_z = format!("file://{}", store_z.path().display());
    let recv = run_with_stdin(
        cache.path(),
        &["receive-pack", "--store", &url_z, "--require-manifest", &id],
        &zstd_bytes,
    );
    assert!(
        recv.status.success(),
        "receive-pack of the zstd stream failed: {}",
        String::from_utf8_lossy(&recv.stderr)
    );
    assert!(
        recv.stdout.is_empty(),
        "receive-pack must print nothing to stdout"
    );

    let man_rel = manifest_path(&id);
    assert_eq!(
        std::fs::read(store_z.path().join(&man_rel)).expect("manifest in Z"),
        std::fs::read(store_a.path().join(&man_rel)).expect("manifest in A"),
        "zstd-decoded manifest must be byte-equal to the v1 source"
    );
    for checksum in &object_ids {
        let rel = object_path(checksum);
        assert_eq!(
            std::fs::read(store_z.path().join(&rel)).expect("object in Z"),
            std::fs::read(store_a.path().join(&rel)).expect("object in A"),
            "zstd-decoded object {rel} must be byte-equal to the v1 source"
        );
    }
}

// ---------------------------------------------------------------------------
// receive-pack security
// ---------------------------------------------------------------------------

/// A hand-crafted stream claiming checksum X over bytes hashing to Y must be
/// rejected: exit != 0, NOTHING filed at X's sharded path, no manifest, no
/// temp litter anywhere in the store.
#[test]
fn plumbing_receive_pack_rejects_hash_mismatch() {
    let cache = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store_url = format!("file://{}", store_dir.path().display());

    let claimed = hex_of(b"good bytes");
    let evil = b"evil bytes"; // same length, different content
    let mut stream = b"SNAPPACK 1\n".to_vec();
    stream.extend_from_slice(format!("obj {claimed} {}\n", evil.len()).as_bytes());
    stream.extend_from_slice(evil);
    stream.extend_from_slice(b"end\n");

    let out = run_with_stdin(
        cache.path(),
        &["receive-pack", "--store", &store_url],
        &stream,
    );
    assert!(!out.status.success(), "mismatched payload must be rejected");
    assert!(out.stdout.is_empty(), "stdout must stay silent");
    assert!(
        !store_dir.path().join(object_path(&claimed)).exists(),
        "nothing may be filed at the claimed checksum's sharded path"
    );
    assert_eq!(
        files_under(store_dir.path()),
        Vec::<std::path::PathBuf>::new(),
        "no file (object, manifest, or temp) may survive the rejected stream"
    );
}

/// A `../`-flavored / non-hex header line is rejected outright (the path is
/// only ever derived from a VALIDATED checksum, so the traversal class is
/// structurally absent — this pins the validation actually firing).
#[test]
fn plumbing_receive_pack_rejects_traversal_and_non_hex_headers() {
    let cache = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store_url = format!("file://{}", store_dir.path().display());

    for bad_checksum in [
        "../../../../etc/passwd",
        "..%2f..%2fescape00000000000000000000000000000000000000000000000000",
        &format!("{}/../x", &"0123456789abcdef".repeat(4)[..32]),
    ] {
        let stream = format!("SNAPPACK 1\nobj {bad_checksum} 0\nend\n");
        let out = run_with_stdin(
            cache.path(),
            &["receive-pack", "--store", &store_url],
            stream.as_bytes(),
        );
        assert!(
            !out.status.success(),
            "header checksum {bad_checksum:?} must be rejected"
        );
        assert_eq!(
            files_under(store_dir.path()),
            Vec::<std::path::PathBuf>::new(),
            "rejected header {bad_checksum:?} must file nothing"
        );
    }
}

/// TRUNCATED stream (cut before `end`, after complete obj records): exit != 0,
/// the verified objects ARE filed (a retry resumes incrementally), the
/// manifest is NOT committed (manifest-last survives truncation). Then —
/// idempotency — re-running the FULL send-pack|receive-pack into the SAME
/// store succeeds and lands the manifest, completing the interrupted push.
#[test]
fn plumbing_receive_pack_truncation_then_full_rerun_completes() {
    let cache = TempDir::new().unwrap();
    let (store_a, id, object_ids) = pushed_fixture(cache.path());
    let store_b = TempDir::new().unwrap();
    let url_a = format!("file://{}", store_a.path().display());
    let url_b = format!("file://{}", store_b.path().display());
    let ids_stdin = format!("{}\n", object_ids.join("\n"));

    // Capture a full valid pack from the sender binary...
    let send = run_with_stdin(
        cache.path(),
        &[
            "send-pack",
            "--store",
            &url_a,
            "--ids",
            "-",
            "--manifest-id",
            &id,
        ],
        ids_stdin.as_bytes(),
    );
    assert!(send.status.success(), "fixture send-pack must succeed");
    let pack = send.stdout;
    assert!(pack.ends_with(b"end\n"), "full pack ends with the trailer");

    // ...and cut it just before the `end` trailer (every obj record + the
    // manifest record are complete; only the commit trigger is missing).
    let cut = &pack[..pack.len() - b"end\n".len()];
    let out = run_with_stdin(
        cache.path(),
        &["receive-pack", "--store", &url_b, "--require-manifest", &id],
        cut,
    );
    assert!(!out.status.success(), "truncated stream must fail");

    // The verified objects ARE filed...
    for checksum in &object_ids {
        let rel = object_path(checksum);
        assert!(
            store_b.path().join(&rel).exists(),
            "verified object {rel} must be filed despite the truncation"
        );
    }
    // ...but the manifest must NOT be committed (manifest-last).
    assert!(
        !store_b.path().join(manifest_path(&id)).exists(),
        "truncated stream must never commit the manifest"
    );

    // Idempotent completion: the FULL pack into the same (partially filled)
    // store succeeds — duplicates verified-then-skipped — and commits the
    // manifest, finishing the interrupted push.
    let out = run_with_stdin(
        cache.path(),
        &["receive-pack", "--store", &url_b, "--require-manifest", &id],
        &pack,
    );
    assert!(
        out.status.success(),
        "full re-run must complete the interrupted push: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let man_rel = manifest_path(&id);
    assert_eq!(
        std::fs::read(store_b.path().join(&man_rel)).expect("manifest in B"),
        std::fs::read(store_a.path().join(&man_rel)).expect("manifest in A"),
        "completed push must land the byte-equal manifest"
    );
}

/// `--require-manifest <other>` against a stream carrying manifest id M fails
/// with a non-zero exit (the committed id is compared, not just "a manifest
/// was committed").
#[test]
fn plumbing_receive_pack_require_manifest_mismatch_fails() {
    let cache = TempDir::new().unwrap();
    let (store_a, id, object_ids) = pushed_fixture(cache.path());
    let store_b = TempDir::new().unwrap();
    let url_a = format!("file://{}", store_a.path().display());
    let url_b = format!("file://{}", store_b.path().display());

    let send = run_with_stdin(
        cache.path(),
        &[
            "send-pack",
            "--store",
            &url_a,
            "--ids",
            "-",
            "--manifest-id",
            &id,
        ],
        format!("{}\n", object_ids.join("\n")).as_bytes(),
    );
    assert!(send.status.success());

    let other = hex_of(b"some other manifest id");
    assert_ne!(other, id);
    let out = run_with_stdin(
        cache.path(),
        &[
            "receive-pack",
            "--store",
            &url_b,
            "--require-manifest",
            &other,
        ],
        &send.stdout,
    );
    assert!(
        !out.status.success(),
        "manifest id mismatch must fail receive-pack"
    );
}

// ---------------------------------------------------------------------------
// send-pack fail-closed
// ---------------------------------------------------------------------------

/// An id absent from the source store fails send-pack (exit != 0) and the
/// emitted bytes do NOT end with `end\n` — so a piped receive-pack of the
/// partial stream fails too (no silent partial transfer). Uses the `--ids
/// <FILE>` form to cover the file-path branch.
#[test]
fn plumbing_send_pack_missing_object_aborts_before_end() {
    let cache = TempDir::new().unwrap();
    let (store_a, _id, mut object_ids) = pushed_fixture(cache.path());
    let url_a = format!("file://{}", store_a.path().display());

    // A syntactically valid but ABSENT object id, appended after real ones.
    object_ids.push(hex_of(b"never stored anywhere"));
    let ids_file = TempDir::new().unwrap();
    ids_file
        .child("ids.txt")
        .write_str(&format!("{}\n", object_ids.join("\n")))
        .unwrap();
    let ids_path = ids_file
        .child("ids.txt")
        .path()
        .to_string_lossy()
        .into_owned();

    let out = snapdir(cache.path())
        .args(["send-pack", "--store", &url_a, "--ids", &ids_path])
        .output()
        .expect("run send-pack");
    assert!(!out.status.success(), "missing object must fail send-pack");
    assert!(
        !out.stdout.ends_with(b"end\n"),
        "the partial stream must NOT carry the end trailer"
    );
}

/// A malformed id in the list fails send-pack BEFORE a single pack byte is
/// emitted (fail closed, same validation as objects-needed).
#[test]
fn plumbing_send_pack_malformed_id_emits_nothing() {
    let cache = TempDir::new().unwrap();
    let (store_a, _id, object_ids) = pushed_fixture(cache.path());
    let url_a = format!("file://{}", store_a.path().display());

    let stdin = format!("{}\nNOT-A-CHECKSUM\n", object_ids[0]);
    let out = run_with_stdin(
        cache.path(),
        &["send-pack", "--store", &url_a, "--ids", "-"],
        stdin.as_bytes(),
    );
    assert!(!out.status.success(), "malformed id must fail send-pack");
    assert!(
        out.stdout.is_empty(),
        "fail closed: not a single pack byte may be emitted"
    );
}

// ---------------------------------------------------------------------------
// SNAPDIR_FSYNC durability knob (env-only; no CLI surface change)
// ---------------------------------------------------------------------------

/// Builds a minimal, VALID single-object pack stream (correct content address,
/// terminated with `end\n`) — the smallest input that drives `receive-pack`
/// all the way through filing + the durability barrier.
fn valid_single_object_pack() -> (Vec<u8>, String) {
    let payload = b"snapdir fsync knob payload";
    let checksum = hex_of(payload);
    let mut stream = b"SNAPPACK 1\n".to_vec();
    stream.extend_from_slice(format!("obj {checksum} {}\n", payload.len()).as_bytes());
    stream.extend_from_slice(payload);
    stream.extend_from_slice(b"end\n");
    (stream, checksum)
}

/// Runs `receive-pack` with `SNAPDIR_FSYNC` either set to `value` (`Some`) or
/// explicitly removed (`None`), feeding `stdin_bytes` in. Mirrors
/// `run_with_stdin` but pins the knob so a leaked parent env can't perturb it.
fn run_recv_with_fsync(
    cache: &Path,
    store_url: &str,
    fsync: Option<&str>,
    stdin_bytes: &[u8],
) -> Output {
    let mut cmd = snapdir(cache);
    cmd.args(["receive-pack", "--store", store_url]);
    match fsync {
        Some(v) => {
            cmd.env("SNAPDIR_FSYNC", v);
        }
        None => {
            cmd.env_remove("SNAPDIR_FSYNC");
        }
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn receive-pack");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(stdin_bytes)
        .expect("write stdin");
    child.wait_with_output().expect("receive-pack output")
}

/// Default (`SNAPDIR_FSYNC` unset) is the batched-durability path: a valid pack
/// is accepted and the object lands byte-equal at its sharded address. This
/// also exercises the end-to-end barrier (`RecordingSink::flush_barrier` must
/// delegate, or the Batch durability would silently no-op).
#[test]
fn plumbing_fsync_default_is_batch_and_files_object() {
    let cache = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store_url = format!("file://{}", store_dir.path().display());
    let (stream, checksum) = valid_single_object_pack();

    let out = run_recv_with_fsync(cache.path(), &store_url, None, &stream);
    assert!(
        out.status.success(),
        "default receive-pack must succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let filed = store_dir.path().join(object_path(&checksum));
    assert!(filed.exists(), "object must be filed at its sharded path");
    assert_eq!(
        std::fs::read(&filed).unwrap(),
        b"snapdir fsync knob payload",
        "filed object must be byte-equal to the payload"
    );
}

/// `SNAPDIR_FSYNC=off` is the historical no-fsync path and is accepted: the
/// same valid pack still files the object correctly.
#[test]
fn plumbing_fsync_off_is_accepted_and_files_object() {
    let cache = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store_url = format!("file://{}", store_dir.path().display());
    let (stream, checksum) = valid_single_object_pack();

    let out = run_recv_with_fsync(cache.path(), &store_url, Some("off"), &stream);
    assert!(
        out.status.success(),
        "SNAPDIR_FSYNC=off must be accepted: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        store_dir.path().join(object_path(&checksum)).exists(),
        "object must be filed under SNAPDIR_FSYNC=off"
    );
}

/// `SNAPDIR_FSYNC=batch` is accepted explicitly (the named default).
#[test]
fn plumbing_fsync_batch_is_accepted_explicitly() {
    let cache = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store_url = format!("file://{}", store_dir.path().display());
    let (stream, checksum) = valid_single_object_pack();

    let out = run_recv_with_fsync(cache.path(), &store_url, Some("batch"), &stream);
    assert!(
        out.status.success(),
        "SNAPDIR_FSYNC=batch must be accepted: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        store_dir.path().join(object_path(&checksum)).exists(),
        "object must be filed under SNAPDIR_FSYNC=batch"
    );
}

/// An unknown `SNAPDIR_FSYNC` value FAILS CLOSED: exit != 0 with a clear
/// message naming the accepted values, and NOTHING is filed (we never silently
/// downgrade durability the operator asked for).
#[test]
fn plumbing_fsync_unknown_value_fails_closed() {
    let cache = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store_url = format!("file://{}", store_dir.path().display());
    let (stream, checksum) = valid_single_object_pack();

    let out = run_recv_with_fsync(cache.path(), &store_url, Some("fsyncall"), &stream);
    assert!(
        !out.status.success(),
        "an unknown SNAPDIR_FSYNC value must fail closed"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("SNAPDIR_FSYNC"),
        "error must name the env var: {stderr}"
    );
    assert!(
        stderr.contains("batch") && stderr.contains("off"),
        "error must name the accepted values `batch`/`off`: {stderr}"
    );
    assert!(
        !store_dir.path().join(object_path(&checksum)).exists(),
        "fail closed: nothing may be filed when the knob is rejected"
    );
}
