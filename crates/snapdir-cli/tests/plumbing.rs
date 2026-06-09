//! Gate tests for the hidden SNAPPACK plumbing subcommands
//! (`version --capabilities`, `objects-needed`, `send-pack`, `receive-pack`)
//! that power the upcoming `ssh://` store acceleration
//! (`<local> snapdir send-pack | ssh host 'snapdir receive-pack'`).
//!
//! Everything here drives the REAL binary (`env!("CARGO_BIN_EXE_snapdir")`)
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
fn snapdir_bin() -> &'static str {
    env!("CARGO_BIN_EXE_snapdir")
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
/// caps=objects-needed,send-pack,receive-pack\n` — pinned both to the literal
/// wire-1 grammar the remote probe matches AND to the lib constants it must
/// bake in (so a constant bump can't silently desync the CLI).
#[test]
fn plumbing_capabilities_line_exact() {
    let cache = TempDir::new().unwrap();
    let out = snapdir(cache.path())
        .args(["version", "--capabilities"])
        .output()
        .expect("run snapdir");
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    // Literal wire-1 pin (the probe negotiates on this exact integer).
    assert_eq!(
        stdout,
        format!(
            "snapdir {} wire=1 caps=objects-needed,send-pack,receive-pack\n",
            env!("CARGO_PKG_VERSION")
        )
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
