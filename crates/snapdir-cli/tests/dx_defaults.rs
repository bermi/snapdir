//! Black-box spec tests for the 1.8.0 `snapdir defaults` REWRITE (phase 30).
//!
//! Today `snapdir defaults` prints ~4 near-useless lines (a binary path twice
//! plus two empty legacy `SNAPDIR_MANIFEST_*` vars) and ignores every override
//! flag — including its own `--cache-dir`. The rewrite makes `defaults` print
//! the REAL effective configuration: for every knob, its RESOLVED value AND a
//! source tag (`flag` | `env` | `default`), reflecting flags and env overrides
//! with correct precedence.
//!
//! These tests pin that NEW contract and are EXPECTED TO FAIL against the
//! current binary (authoring mode — the implementation does not exist yet).
//! They are deliberately black-box: they drive only the public CLI via
//! `assert_cmd` and never read `src/`.
//!
//! Each test is hermetic: it clears the inherited environment and sets its own
//! clean `PATH`, a temp `HOME`, and a temp `--cache-dir`, so results are
//! deterministic and never leak the developer's real `SNAPDIR*` env.
//!
//! IMPORTANT FORMAT LATITUDE: the impl chooses the exact whitespace / column
//! layout. These tests pin the SUBSTANCE only — presence of a knob name, an
//! associated resolved value, and a source tag on the SAME line — via
//! case-insensitive, line-oriented `contains` checks. They do NOT pin byte
//! columns. Every fn name contains `dx_defaults` so
//! `cargo test -p snapdir-cli --locked dx_defaults` selects exactly this suite.

use std::process::Command;

use assert_cmd::prelude::*;
use assert_fs::TempDir;

/// A `snapdir` command with the inherited environment fully cleared, then only
/// the bare minimum re-set: `PATH` (so the loader works) and a temp `HOME`
/// (so nothing resolves into the developer's real home). Tests add the
/// `SNAPDIR*` vars and flags they want — nothing leaks in from the host, so
/// the output is fully deterministic.
fn snapdir_clean(home: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("snapdir").expect("snapdir binary built");
    cmd.env_clear();
    if let Ok(path) = std::env::var("PATH") {
        cmd.env("PATH", path);
    }
    cmd.env("HOME", home.path());
    cmd
}

/// Runs `snapdir defaults <extra-args>` on the clean env, asserts success, and
/// returns its stdout split into lines.
fn defaults_lines(cmd: &mut Command, extra: &[&str]) -> Vec<String> {
    let out = cmd
        .arg("defaults")
        .args(extra)
        .output()
        .expect("run snapdir defaults");
    assert!(
        out.status.success(),
        "snapdir defaults {extra:?} failed ({:?})\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8(out.stdout)
        .expect("utf8 stdout")
        .lines()
        .map(ToOwned::to_owned)
        .collect()
}

/// Raw (success-asserted) stdout bytes of `snapdir defaults <extra>`.
fn defaults_stdout(cmd: &mut Command, extra: &[&str]) -> String {
    let out = cmd
        .arg("defaults")
        .args(extra)
        .output()
        .expect("run snapdir defaults");
    assert!(
        out.status.success(),
        "snapdir defaults {extra:?} failed ({:?})\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8(out.stdout).expect("utf8 stdout")
}

/// True if some line, lowercased, mentions `knob` AND contains the (lowercased)
/// `value` substring — i.e. the knob is reported with that value ON ONE LINE.
/// Knob-name matching tolerates `-`/`_` spelling so the impl may print either
/// `cache-dir` or `cache_dir`.
fn line_assocs(lines: &[String], knob: &str, value: &str) -> bool {
    let knob_us = knob.replace('-', "_");
    let val = value.to_lowercase();
    lines.iter().any(|l| {
        let low = l.to_lowercase();
        (low.contains(knob) || low.contains(&knob_us)) && low.contains(&val)
    })
}

/// True if some line names `knob` at all (either `-` or `_` spelling).
fn line_has_knob(lines: &[String], knob: &str) -> bool {
    let knob_us = knob.replace('-', "_");
    lines.iter().any(|l| {
        let low = l.to_lowercase();
        low.contains(knob) || low.contains(&knob_us)
    })
}

/// The single line that names `knob` (panics if none / many candidates is fine
/// — returns the first match), for tag/value assertions scoped to that knob.
fn knob_line(lines: &[String], knob: &str) -> String {
    let knob_us = knob.replace('-', "_");
    lines
        .iter()
        .find(|l| {
            let low = l.to_lowercase();
            low.contains(knob) || low.contains(&knob_us)
        })
        .cloned()
        .unwrap_or_else(|| panic!("no line names knob `{knob}` in:\n{}", lines.join("\n")))
}

/// The representative knob set the rewrite MUST surface on a clean env. The
/// spec names a long list; this is the strong, must-appear subset (it also
/// covers the more exotic ones the spec calls non-negotiable).
const REQUIRED_KNOBS: &[&str] = &[
    "cache-dir",
    "store",
    "catalog",
    "jobs",
    "walk-jobs",
    "color",
    "no-progress",
    "fsync",
    "clonefile",
];

// ---------------------------------------------------------------------------
// Clause 1: every effective knob is printed on a clean env, with value + tag.
// ---------------------------------------------------------------------------

/// Clause 1: on a clean env, the representative knob set is each PRESENT.
#[test]
fn dx_defaults_clean_env_lists_every_required_knob() {
    let home = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let mut cmd = snapdir_clean(&home);
    cmd.env("SNAPDIR_CACHE_DIR", cache.path());
    let lines = defaults_lines(&mut cmd, &[]);

    for knob in REQUIRED_KNOBS {
        assert!(
            line_has_knob(&lines, knob),
            "clean-env defaults must list knob `{knob}` in:\n{}",
            lines.join("\n"),
        );
    }
}

/// Clause 1 (broader list): the wider knob set from the spec also appears.
/// `objects-store`, the adaptive/retry/request batch knobs, verify-copies.
#[test]
fn dx_defaults_clean_env_lists_extended_knob_set() {
    let home = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let mut cmd = snapdir_clean(&home);
    cmd.env("SNAPDIR_CACHE_DIR", cache.path());
    let lines = defaults_lines(&mut cmd, &[]);

    for knob in [
        "objects-store",
        "limit-rate",
        "adaptive",
        "max-jobs",
        "max-retries",
        "retry-base-ms",
        "retry-max-ms",
        "max-requests",
        "verify-copies",
    ] {
        assert!(
            line_has_knob(&lines, knob),
            "clean-env defaults must list extended knob `{knob}` in:\n{}",
            lines.join("\n"),
        );
    }
}

// ---------------------------------------------------------------------------
// Clause 2: a source tag (flag|env|default) is present; clean env => default.
// ---------------------------------------------------------------------------

/// Clause 2: the literal source-tag vocabulary appears in the output.
#[test]
fn dx_defaults_source_tag_vocabulary_present() {
    let home = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let mut cmd = snapdir_clean(&home);
    cmd.env("SNAPDIR_CACHE_DIR", cache.path());
    let out = defaults_stdout(&mut cmd, &[]).to_lowercase();

    // On a clean env at least the `default` tag must appear (auto/unset knobs).
    assert!(
        out.contains("default"),
        "a `default` source tag must appear on a clean env:\n{out}",
    );
}

/// Clause 2: on a clean env, an auto/unset knob (`jobs`) is tagged `default`.
#[test]
fn dx_defaults_clean_env_auto_jobs_tagged_default() {
    let home = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let mut cmd = snapdir_clean(&home);
    cmd.env("SNAPDIR_CACHE_DIR", cache.path());
    let lines = defaults_lines(&mut cmd, &[]);

    let jobs = knob_line(&lines, "jobs").to_lowercase();
    assert!(
        jobs.contains("default"),
        "clean-env `jobs` (auto-resolved) must be tagged `default`, got: {jobs:?}",
    );
}

/// Clause 2: a knob with a hard default (`clonefile`) is tagged `default` on a
/// clean env (it is not overridden, so its source is the built-in default).
#[test]
fn dx_defaults_clean_env_clonefile_tagged_default() {
    let home = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let mut cmd = snapdir_clean(&home);
    cmd.env("SNAPDIR_CACHE_DIR", cache.path());
    let lines = defaults_lines(&mut cmd, &[]);

    let clonefile = knob_line(&lines, "clonefile").to_lowercase();
    assert!(
        clonefile.contains("default"),
        "clean-env `clonefile` must be tagged `default`, got: {clonefile:?}",
    );
}

// ---------------------------------------------------------------------------
// Clause 3: RESOLVED values, not just names.
// ---------------------------------------------------------------------------

/// Clause 3: `jobs` / `walk-jobs` show a concrete number (auto-resolved CPU
/// count), not the literal word "auto" or an empty value.
#[test]
fn dx_defaults_jobs_resolved_to_concrete_number() {
    let home = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let mut cmd = snapdir_clean(&home);
    cmd.env("SNAPDIR_CACHE_DIR", cache.path());
    let lines = defaults_lines(&mut cmd, &[]);

    for knob in ["jobs", "walk-jobs"] {
        let line = knob_line(&lines, knob);
        assert!(
            line.chars().any(|c| c.is_ascii_digit()),
            "`{knob}` must show a concrete resolved number, got: {line:?}",
        );
    }
}

/// Clause 3: `cache-dir` shows a concrete filesystem path (contains a `/`),
/// here the temp cache we pinned via env.
#[test]
fn dx_defaults_cache_dir_resolved_to_concrete_path() {
    let home = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let mut cmd = snapdir_clean(&home);
    cmd.env("SNAPDIR_CACHE_DIR", cache.path());
    let lines = defaults_lines(&mut cmd, &[]);

    let line = knob_line(&lines, "cache-dir");
    assert!(
        line.contains('/'),
        "`cache-dir` must show a concrete path, got: {line:?}",
    );
    // And specifically the env-pinned cache path's leaf is reflected somewhere.
    let leaf = cache
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(
        lines.iter().any(|l| l.contains(&leaf)),
        "the pinned cache dir `{leaf}` must be reflected in:\n{}",
        lines.join("\n"),
    );
}

/// Clause 3: `clonefile` shows its enabled/true default value (not just the
/// name) and `fsync` shows its `batch` default value.
#[test]
fn dx_defaults_clonefile_and_fsync_show_default_values() {
    let home = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let mut cmd = snapdir_clean(&home);
    cmd.env("SNAPDIR_CACHE_DIR", cache.path());
    let lines = defaults_lines(&mut cmd, &[]);

    // clonefile defaults ON: accept any enabled spelling (true/on/enabled/yes).
    let clonefile = knob_line(&lines, "clonefile").to_lowercase();
    assert!(
        ["true", "on", "enabled", "yes"]
            .iter()
            .any(|v| clonefile.contains(v)),
        "`clonefile` must show its enabled default value, got: {clonefile:?}",
    );

    // fsync defaults to `batch`.
    let fsync = knob_line(&lines, "fsync").to_lowercase();
    assert!(
        fsync.contains("batch"),
        "`fsync` must show its `batch` default value, got: {fsync:?}",
    );
}

// ---------------------------------------------------------------------------
// Clause 4: flag override reflected with source=flag (the prior bug).
// ---------------------------------------------------------------------------

/// Clause 4: `--cache-dir /tmp/dxc-XYZ` is reflected with the flag value AND
/// tagged `flag`. (The OLD bug: `defaults` ignored its own `--cache-dir`.)
#[test]
fn dx_defaults_cache_dir_flag_override_tagged_flag() {
    let home = TempDir::new().unwrap();
    let flag_dir = "/tmp/dxc-XYZ-adversary";
    let mut cmd = snapdir_clean(&home);
    let lines = defaults_lines(&mut cmd, &["--cache-dir", flag_dir]);

    assert!(
        line_assocs(&lines, "cache-dir", flag_dir),
        "`--cache-dir {flag_dir}` must be reflected as the cache-dir value in:\n{}",
        lines.join("\n"),
    );
    let line = knob_line(&lines, "cache-dir").to_lowercase();
    assert!(
        line.contains("flag"),
        "flag-overridden `cache-dir` must be tagged `flag`, got: {line:?}",
    );
}

/// Clause 4: `--jobs 3` => jobs shows `3`, tagged `flag`.
#[test]
fn dx_defaults_jobs_flag_override_tagged_flag() {
    let home = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let mut cmd = snapdir_clean(&home);
    cmd.env("SNAPDIR_CACHE_DIR", cache.path());
    let lines = defaults_lines(&mut cmd, &["--jobs", "3"]);

    assert!(
        line_assocs(&lines, "jobs", "3"),
        "`--jobs 3` must show jobs=3 in:\n{}",
        lines.join("\n"),
    );
    let line = knob_line(&lines, "jobs").to_lowercase();
    assert!(
        line.contains("flag"),
        "flag-overridden `jobs` must be tagged `flag`, got: {line:?}",
    );
}

/// Clause 4 (anti-regression of the exact prior bug): `defaults --cache-dir X`
/// is NOT byte-identical to bare `defaults` — the flag actually changes output.
#[test]
fn dx_defaults_cache_dir_flag_changes_output() {
    let home = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();

    let mut bare = snapdir_clean(&home);
    bare.env("SNAPDIR_CACHE_DIR", cache.path());
    let bare_out = defaults_stdout(&mut bare, &[]);

    let mut flagged = snapdir_clean(&home);
    flagged.env("SNAPDIR_CACHE_DIR", cache.path());
    let flagged_out = defaults_stdout(&mut flagged, &["--cache-dir", "/tmp/dxc-DIFFERENT"]);

    assert_ne!(
        bare_out, flagged_out,
        "`defaults --cache-dir <X>` must differ from bare `defaults` (the prior bug)",
    );
}

// ---------------------------------------------------------------------------
// Clause 5: env override reflected with source=env.
// ---------------------------------------------------------------------------

/// Clause 5: `SNAPDIR_JOBS=7 defaults` => jobs shows `7`, tagged `env`.
#[test]
fn dx_defaults_jobs_env_override_tagged_env() {
    let home = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let mut cmd = snapdir_clean(&home);
    cmd.env("SNAPDIR_CACHE_DIR", cache.path());
    cmd.env("SNAPDIR_JOBS", "7");
    let lines = defaults_lines(&mut cmd, &[]);

    assert!(
        line_assocs(&lines, "jobs", "7"),
        "`SNAPDIR_JOBS=7` must show jobs=7 in:\n{}",
        lines.join("\n"),
    );
    let line = knob_line(&lines, "jobs").to_lowercase();
    assert!(
        line.contains("env"),
        "env-overridden `jobs` must be tagged `env`, got: {line:?}",
    );
}

/// Clause 5: `SNAPDIR_STORE=file://x defaults` => store shows that URL, tagged
/// `env`.
#[test]
fn dx_defaults_store_env_override_tagged_env() {
    let home = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let store = "file:///tmp/dx-env-store";
    let mut cmd = snapdir_clean(&home);
    cmd.env("SNAPDIR_CACHE_DIR", cache.path());
    cmd.env("SNAPDIR_STORE", store);
    let lines = defaults_lines(&mut cmd, &[]);

    assert!(
        line_assocs(&lines, "store", store),
        "`SNAPDIR_STORE={store}` must be reflected as the store value in:\n{}",
        lines.join("\n"),
    );
    // The store line (not objects-store) must be tagged env. Pick the line that
    // mentions the URL to scope the tag assertion precisely.
    let store_line = lines
        .iter()
        .find(|l| l.contains(store))
        .cloned()
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        store_line.contains("env"),
        "env-set `store` must be tagged `env`, got: {store_line:?}",
    );
}

// ---------------------------------------------------------------------------
// Clause 6: precedence — flag beats env.
// ---------------------------------------------------------------------------

/// Clause 6: `SNAPDIR_JOBS=7 defaults --jobs 3` => jobs=3, tagged `flag`
/// (flag wins over env; the env value 7 is NOT the reported jobs value).
#[test]
fn dx_defaults_flag_beats_env_for_jobs() {
    let home = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let mut cmd = snapdir_clean(&home);
    cmd.env("SNAPDIR_CACHE_DIR", cache.path());
    cmd.env("SNAPDIR_JOBS", "7");
    let lines = defaults_lines(&mut cmd, &["--jobs", "3"]);

    let line = knob_line(&lines, "jobs").to_lowercase();
    assert!(
        line.contains('3'),
        "flag --jobs 3 must win over SNAPDIR_JOBS=7, got jobs line: {line:?}",
    );
    assert!(
        line.contains("flag"),
        "flag-winning `jobs` must be tagged `flag`, got: {line:?}",
    );
    // The env value 7 must NOT be the resolved jobs value.
    assert!(
        !line.contains('7'),
        "the overridden env value 7 must not be the reported jobs value: {line:?}",
    );
}

// ---------------------------------------------------------------------------
// Clause 7: arbitrary set SNAPDIR_* is still surfaced (superset, no regression).
// ---------------------------------------------------------------------------

/// Clause 7: a set `SNAPDIR_*` var (here a recognized knob via env) is still
/// reflected — the new output is a SUPERSET of the old "echo env" behavior.
#[test]
fn dx_defaults_still_surfaces_set_snapdir_env_var() {
    let home = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let mut cmd = snapdir_clean(&home);
    cmd.env("SNAPDIR_CACHE_DIR", cache.path());
    cmd.env("SNAPDIR_CATALOG", "my-catalog-name");
    let lines = defaults_lines(&mut cmd, &[]);

    assert!(
        lines.iter().any(|l| l.contains("my-catalog-name")),
        "a set SNAPDIR_CATALOG must still be reflected in:\n{}",
        lines.join("\n"),
    );
}

// ---------------------------------------------------------------------------
// Clause 8: legacy bash cruft (SNAPDIR_MANIFEST_*) not presented as live knobs.
// ---------------------------------------------------------------------------

/// Clause 8: on a clean env, the legacy `SNAPDIR_MANIFEST_CONTEXT` /
/// `SNAPDIR_MANIFEST_EXCLUDE` are NOT presented as active effective knobs
/// (the old output emitted empty `SNAPDIR_MANIFEST_*=` lines). If they appear
/// at all they must be explicitly labeled legacy/compat — not bare entries in
/// the effective-knob list.
#[test]
fn dx_defaults_legacy_manifest_vars_not_live_knobs() {
    let home = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let mut cmd = snapdir_clean(&home);
    cmd.env("SNAPDIR_CACHE_DIR", cache.path());
    let lines = defaults_lines(&mut cmd, &[]);

    for legacy in ["SNAPDIR_MANIFEST_CONTEXT", "SNAPDIR_MANIFEST_EXCLUDE"] {
        for line in lines.iter().filter(|l| l.contains(legacy)) {
            let low = line.to_lowercase();
            assert!(
                low.contains("legacy") || low.contains("compat") || low.contains("deprecat"),
                "legacy `{legacy}` must not appear as a live knob; if present it \
                 must be labeled legacy/compat, got: {line:?}",
            );
        }
        // The old empty-value line shape (`SNAPDIR_MANIFEST_*=` with nothing
        // after) must be gone — that was the useless legacy cruft.
        let empty = format!("{legacy}=");
        assert!(
            !lines.iter().any(|l| l.trim() == empty),
            "the old empty `{empty}` legacy line must not appear in:\n{}",
            lines.join("\n"),
        );
    }
}

// ---------------------------------------------------------------------------
// Clause 9: deterministic + parseable.
// ---------------------------------------------------------------------------

/// Clause 9: two runs on the SAME env are byte-identical (deterministic).
#[test]
fn dx_defaults_two_runs_byte_identical() {
    let home = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();

    let mut a = snapdir_clean(&home);
    a.env("SNAPDIR_CACHE_DIR", cache.path());
    a.env("SNAPDIR_STORE", "file:///tmp/dx-det");
    let out_a = defaults_stdout(&mut a, &[]);

    let mut b = snapdir_clean(&home);
    b.env("SNAPDIR_CACHE_DIR", cache.path());
    b.env("SNAPDIR_STORE", "file:///tmp/dx-det");
    let out_b = defaults_stdout(&mut b, &[]);

    assert_eq!(
        out_a, out_b,
        "two `defaults` runs on the same env must be byte-identical",
    );
}

/// Clause 9: the output is line-oriented / greppable — for every required knob
/// the knob's line carries BOTH a non-empty value region and a recognized
/// source tag, i.e. a stable `<knob> ... <value> ... <tag>`-style shape that a
/// simple grep can parse. (No column pinning — substance only.)
#[test]
fn dx_defaults_each_required_knob_line_has_value_and_tag() {
    let home = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let mut cmd = snapdir_clean(&home);
    cmd.env("SNAPDIR_CACHE_DIR", cache.path());
    let lines = defaults_lines(&mut cmd, &[]);

    for knob in REQUIRED_KNOBS {
        let line = knob_line(&lines, knob);
        let low = line.to_lowercase();

        // A recognized source tag is on the knob's own line.
        assert!(
            low.contains("flag") || low.contains("env") || low.contains("default"),
            "knob `{knob}` line must carry a source tag (flag|env|default), got: {line:?}",
        );

        // There is a value region: strip the knob name and a tag word, and at
        // least one non-space, non-separator character remains (the value).
        let stripped: String = low
            .replace(knob, " ")
            .replace(&knob.replace('-', "_"), " ")
            .replace("default", " ")
            .replace("flag", " ")
            .replace("env", " ");
        assert!(
            stripped
                .chars()
                .any(|c| c.is_alphanumeric() || c == '/' || c == '.'),
            "knob `{knob}` line must carry a resolved VALUE beyond its name+tag, got: {line:?}",
        );
    }
}

/// Clause 9 (shape): every non-blank stdout line is reasonably short and
/// single-line (line-oriented, not a JSON blob or paragraph), so the output
/// stays greppable. This pins "line-oriented" without pinning columns.
#[test]
fn dx_defaults_output_is_line_oriented() {
    let home = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let mut cmd = snapdir_clean(&home);
    cmd.env("SNAPDIR_CACHE_DIR", cache.path());
    let out = defaults_stdout(&mut cmd, &[]);

    // Multiple lines (it lists many knobs), and no embedded NULs.
    let non_blank: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(
        non_blank.len() >= REQUIRED_KNOBS.len(),
        "expected at least one line per required knob, got {} lines:\n{out}",
        non_blank.len(),
    );
    assert!(
        !out.contains('\0'),
        "output must not contain NUL bytes (line-oriented text)",
    );
}

// ===========================================================================
// Impl-revealed cases (phase 30 review — implementation now visible).
//
// The spec tests above deliberately kept FORMAT LATITUDE (case-insensitive
// `contains` on substance). Now that `9c31b95` landed, the exact tokens are
// known and pinned below so they cannot silently drift:
//   * source tag literal is `source=<flag|env|default>` (one token, `=`-joined);
//   * the superset section header is the literal `other-env:` and each entry is
//     `  SNAPDIR_KEY=value`, with legacy manifest vars suffixed ` (legacy)`;
//   * resolved default values: clonefile=`enabled`, fsync=`batch`,
//     verify-copies=`disabled`, and their env flips.
// Every fn name still contains `dx_defaults` so the suite selector picks them up.
// These ADDED tests MUST PASS against the current binary.
// ===========================================================================

/// All three literal source tokens — `source=default`, `source=env`,
/// `source=flag` — appear with the exact `source=<tag>` spelling (no spaces
/// around `=`, lowercase tag). Pins the format the impl chose.
#[test]
fn dx_defaults_literal_source_tokens_exact() {
    let home = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let mut cmd = snapdir_clean(&home);
    // env-set cache-dir → at least one `source=env`; `--jobs` → `source=flag`;
    // every unset knob → `source=default`.
    cmd.env("SNAPDIR_CACHE_DIR", cache.path());
    let out = defaults_stdout(&mut cmd, &["--jobs", "4"]);

    for token in ["source=default", "source=env", "source=flag"] {
        assert!(
            out.contains(token),
            "expected literal `{token}` in defaults output:\n{out}",
        );
    }
    // And the tag is never printed with surrounding spaces (e.g. `source = env`).
    assert!(
        !out.contains("source ="),
        "source tag must be the tight `source=<tag>` token, not `source = …`:\n{out}",
    );
}

/// The superset section header is the literal `other-env:`, and an arbitrary
/// set `SNAPDIR_*` var (here `SNAPDIR_FOO=bar`, which is NOT a recognized knob)
/// is listed verbatim under it.
#[test]
fn dx_defaults_other_env_section_lists_arbitrary_snapdir_var() {
    let home = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let mut cmd = snapdir_clean(&home);
    cmd.env("SNAPDIR_CACHE_DIR", cache.path());
    cmd.env("SNAPDIR_FOO", "bar");
    let lines = defaults_lines(&mut cmd, &[]);

    assert!(
        lines.iter().any(|l| l == "other-env:"),
        "expected a literal `other-env:` superset header in:\n{}",
        lines.join("\n"),
    );
    assert!(
        lines.iter().any(|l| l.contains("SNAPDIR_FOO=bar")),
        "the arbitrary set `SNAPDIR_FOO=bar` must be listed under other-env in:\n{}",
        lines.join("\n"),
    );
    // It is NOT presented as a recognized effective knob (no `source=` tag on it).
    let foo_line = lines
        .iter()
        .find(|l| l.contains("SNAPDIR_FOO=bar"))
        .expect("the SNAPDIR_FOO line");
    assert!(
        !foo_line.contains("source="),
        "an unrecognized SNAPDIR_* var must be raw env, not a tagged knob: {foo_line:?}",
    );
}

/// A set legacy `SNAPDIR_MANIFEST_CONTEXT` is surfaced ONLY under `other-env:`
/// with the explicit ` (legacy)` suffix — never as a live `source=`-tagged knob.
#[test]
fn dx_defaults_legacy_manifest_context_surfaced_as_legacy_not_knob() {
    let home = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let mut cmd = snapdir_clean(&home);
    cmd.env("SNAPDIR_CACHE_DIR", cache.path());
    cmd.env("SNAPDIR_MANIFEST_CONTEXT", "mykey");
    let lines = defaults_lines(&mut cmd, &[]);

    let manifest = lines
        .iter()
        .find(|l| l.contains("SNAPDIR_MANIFEST_CONTEXT"))
        .expect("a set SNAPDIR_MANIFEST_CONTEXT must still be surfaced");
    assert!(
        manifest.contains("mykey") && manifest.contains("(legacy)"),
        "legacy manifest var must carry its value and the `(legacy)` label: {manifest:?}",
    );
    assert!(
        !manifest.contains("source="),
        "legacy manifest var must NOT appear as a `source=`-tagged effective knob: {manifest:?}",
    );
}

/// Resolved-value sanity for `clonefile`: `enabled` + `source=default` by
/// default, flipped to `disabled` + `source=env` by `SNAPDIR_CLONEFILE=0`.
#[test]
fn dx_defaults_clonefile_default_and_env_flip() {
    let home = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();

    let mut on = snapdir_clean(&home);
    on.env("SNAPDIR_CACHE_DIR", cache.path());
    let on_lines = defaults_lines(&mut on, &[]);
    let on_line = knob_line(&on_lines, "clonefile");
    assert!(
        on_line.contains("enabled") && on_line.contains("source=default"),
        "default clonefile must be `enabled source=default`, got: {on_line:?}",
    );

    let mut off = snapdir_clean(&home);
    off.env("SNAPDIR_CACHE_DIR", cache.path());
    off.env("SNAPDIR_CLONEFILE", "0");
    let off_lines = defaults_lines(&mut off, &[]);
    let off_line = knob_line(&off_lines, "clonefile");
    assert!(
        off_line.contains("disabled") && off_line.contains("source=env"),
        "SNAPDIR_CLONEFILE=0 must flip clonefile to `disabled source=env`, got: {off_line:?}",
    );
}

/// Resolved-value sanity for `fsync`: `batch` + `source=default` by default,
/// flipped to `off` + `source=env` by `SNAPDIR_FSYNC=off`.
#[test]
fn dx_defaults_fsync_default_and_env_flip() {
    let home = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();

    let mut def = snapdir_clean(&home);
    def.env("SNAPDIR_CACHE_DIR", cache.path());
    let def_line = knob_line(&defaults_lines(&mut def, &[]), "fsync");
    assert!(
        def_line.contains("batch") && def_line.contains("source=default"),
        "default fsync must be `batch source=default`, got: {def_line:?}",
    );

    let mut off = snapdir_clean(&home);
    off.env("SNAPDIR_CACHE_DIR", cache.path());
    off.env("SNAPDIR_FSYNC", "off");
    let off_line = knob_line(&defaults_lines(&mut off, &[]), "fsync");
    assert!(
        off_line.contains("off") && off_line.contains("source=env"),
        "SNAPDIR_FSYNC=off must flip fsync to `off source=env`, got: {off_line:?}",
    );
}

/// Resolved-value sanity for `verify-copies`: `disabled` + `source=default` by
/// default, flipped to `enabled` + `source=env` by `SNAPDIR_VERIFY_COPIES=1`.
#[test]
fn dx_defaults_verify_copies_default_and_env_flip() {
    let home = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();

    let mut def = snapdir_clean(&home);
    def.env("SNAPDIR_CACHE_DIR", cache.path());
    let def_line = knob_line(&defaults_lines(&mut def, &[]), "verify-copies");
    assert!(
        def_line.contains("disabled") && def_line.contains("source=default"),
        "default verify-copies must be `disabled source=default`, got: {def_line:?}",
    );

    let mut on = snapdir_clean(&home);
    on.env("SNAPDIR_CACHE_DIR", cache.path());
    on.env("SNAPDIR_VERIFY_COPIES", "1");
    let on_line = knob_line(&defaults_lines(&mut on, &[]), "verify-copies");
    assert!(
        on_line.contains("enabled") && on_line.contains("source=env"),
        "SNAPDIR_VERIFY_COPIES=1 must flip verify-copies to `enabled source=env`, got: {on_line:?}",
    );
}

/// `objects-store` reflects a `--objects-store` flag with `source=flag`, and a
/// `SNAPDIR_OBJECTS_STORE` env with `source=env` — scoped to the objects-store
/// line (distinct from the plain `store` line).
#[test]
fn dx_defaults_objects_store_flag_and_env_source() {
    let home = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();

    // Flag → source=flag.
    let mut flagged = snapdir_clean(&home);
    flagged.env("SNAPDIR_CACHE_DIR", cache.path());
    let flag_lines = defaults_lines(
        &mut flagged,
        &["--objects-store", "file:///tmp/dx-obj-flag"],
    );
    let flag_line = knob_line(&flag_lines, "objects-store");
    assert!(
        flag_line.contains("file:///tmp/dx-obj-flag") && flag_line.contains("source=flag"),
        "`--objects-store …` must show that URL tagged source=flag, got: {flag_line:?}",
    );

    // Env → source=env.
    let mut enved = snapdir_clean(&home);
    enved.env("SNAPDIR_CACHE_DIR", cache.path());
    enved.env("SNAPDIR_OBJECTS_STORE", "file:///tmp/dx-obj-env");
    let env_lines = defaults_lines(&mut enved, &[]);
    let env_line = knob_line(&env_lines, "objects-store");
    assert!(
        env_line.contains("file:///tmp/dx-obj-env") && env_line.contains("source=env"),
        "`SNAPDIR_OBJECTS_STORE=…` must show that URL tagged source=env, got: {env_line:?}",
    );
}
