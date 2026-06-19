//! ADVERSARY black-box spec-tests for the PURE exact-mirror prune-set
//! (Phase 32: `--delete` for checkout/pull/sync).
//!
//! These tests pin the CONTRACT for computing the *extraneous* set: given a
//! target [`Manifest`] and a listing of the destination tree's paths + types,
//! the prune-set is the set of destination paths that are present in the dest
//! but ABSENT from the manifest — i.e. exactly what `--delete` must remove to
//! make the dest an exact mirror of the manifest.
//!
//! Authored from the gate SPEC alone, with ZERO visibility into the (not yet
//! existing) implementation. They will NOT compile/pass until the
//! `mirror-prune-set-impl` gate lands the `snapdir_core::mirror` module.
//!
//! ## Assumed public API (the core impl lane must honor or re-point)
//!
//! ```ignore
//! pub mod mirror {
//!     /// A single destination-tree entry: its path (verbatim, `./`-prefixed
//!     /// like a manifest path; directories end with `/`) and its on-disk type.
//!     #[derive(Debug, Clone, PartialEq, Eq)]
//!     pub struct DestEntry {
//!         pub path: String,
//!         pub path_type: snapdir_core::PathType,
//!     }
//!     impl DestEntry {
//!         pub fn new(path: impl Into<String>, path_type: snapdir_core::PathType) -> Self;
//!     }
//!
//!     /// Computes the extraneous dest paths in DEEPEST-FIRST deletion order.
//!     ///
//!     /// PURE + I/O-light: compares manifest entries (path+type) against the
//!     /// dest listing. NEVER reads/hashes object content; works with no
//!     /// `.objects/` pool present. `excludes` are extended-regex patterns
//!     /// (grep -E semantics, same as `snapdir_core::excludes::ExcludeMatcher`):
//!     /// an extraneous path matching ANY exclude is PROTECTED (omitted).
//!     pub fn prune_set(
//!         manifest: &snapdir_core::Manifest,
//!         dest_entries: &[DestEntry],
//!         excludes: &[&str],
//!     ) -> Vec<String>;
//! }
//! ```
//!
//! NOTE for the impl lane: the SPEC writes `--exclude <glob>`, but the only
//! exclude primitive `snapdir-core` exposes is the extended-regex
//! `excludes::ExcludeMatcher` (grep -E -v). These tests therefore use exclude
//! patterns that read identically as a literal substring AND as a regex (plain
//! path fragments, no metacharacters), so the assertions hold whichever
//! matching primitive the impl wires in. If the impl chooses a different
//! exclude type, re-point the type — do NOT weaken the protect/keep assertions.

use snapdir_core::mirror::{prune_set, DestEntry};
use snapdir_core::{Manifest, PathType};

// ---------------------------------------------------------------------------
// Test helpers (NOT under test): build manifests + dest listings tersely.
// ---------------------------------------------------------------------------

/// A manifest file entry line. Checksums/sizes are deliberately BOGUS — the
/// prune-set must never consult them (it keys on path + type only).
fn mf(path: &str) -> String {
    format!("F 600 deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef 0 {path}")
}

/// A manifest directory entry line (path must end with `/`).
fn md(path: &str) -> String {
    format!("D 700 deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef 0 {path}")
}

/// Parse manifest text from a slice of lines.
fn manifest(lines: &[String]) -> Manifest {
    Manifest::parse(&lines.join("\n")).expect("test manifest parses")
}

/// Convenience: a dest file entry.
fn df(path: &str) -> DestEntry {
    DestEntry::new(path, PathType::File)
}

/// Convenience: a dest directory entry (path should end with `/`).
fn dd(path: &str) -> DestEntry {
    DestEntry::new(path, PathType::Directory)
}

/// True iff `set` is in valid deepest-first deletion order: for every pair
/// where `a` is an ancestor path of `b` (b starts with a, both dirs), `b`
/// (the descendant) must appear BEFORE `a`. Proves children unlink before
/// parents so an `rmdir` of a non-empty dir never happens.
fn is_deepest_first(set: &[String]) -> bool {
    for (i, ancestor) in set.iter().enumerate() {
        // Only directory paths can be ancestors (they end with '/').
        if !ancestor.ends_with('/') {
            continue;
        }
        for (j, descendant) in set.iter().enumerate() {
            if i == j {
                continue;
            }
            // `descendant` lives under `ancestor` and is not the ancestor itself.
            if descendant.starts_with(ancestor.as_str()) && descendant != ancestor {
                // The descendant must be removed first => earlier index.
                if j > i {
                    return false;
                }
            }
        }
    }
    true
}

// ===========================================================================
// CORE CONTRACT: extraneous = in dest, not in manifest
// ===========================================================================

#[test]
fn extraneous_is_dest_paths_absent_from_manifest() {
    // SPEC: "Extraneous = in dest, not in manifest."
    let m = manifest(&[md("./"), mf("./keep.txt")]);
    let dest = vec![dd("./"), df("./keep.txt"), df("./extra.txt")];
    let set = prune_set(&m, &dest, &[]);
    assert!(
        set.contains(&"./extra.txt".to_string()),
        "a dest path absent from the manifest must be extraneous; got {set:?}"
    );
    assert!(
        !set.contains(&"./keep.txt".to_string()),
        "a dest path present in the manifest must NOT be pruned; got {set:?}"
    );
    assert!(
        !set.contains(&"./".to_string()),
        "the root, present in the manifest, must never be pruned; got {set:?}"
    );
}

#[test]
fn manifest_path_kept_even_when_content_differs() {
    // SPEC: "A path present in the manifest is kept (never pruned), regardless
    // of content differences (content repair is materialize's job, not
    // prune's)." The dest file at ./same has a different (in-tree) content than
    // the manifest, but since prune ignores content it must NOT be extraneous.
    let m = manifest(&[md("./"), mf("./same")]);
    // Same path + same type; prune does not look at checksum/size at all.
    let dest = vec![dd("./"), df("./same")];
    let set = prune_set(&m, &dest, &[]);
    assert!(
        set.is_empty(),
        "a path in both manifest and dest is never pruned regardless of content; got {set:?}"
    );
}

// ===========================================================================
// PURITY / I/O-LIGHTNESS: no object pool required
// ===========================================================================

#[test]
fn prune_set_computes_with_no_objects_pool_present() {
    // SPEC: "the prune-set is computable with no `.objects` pool present" — the
    // computation is PURE: it never reads/hashes object content. We prove this
    // by feeding bogus checksums and never providing any store/objects path to
    // the function: its signature takes only (manifest, dest_entries, excludes).
    let m = manifest(&[md("./"), mf("./a.txt")]);
    let dest = vec![dd("./"), df("./a.txt"), df("./b.txt")];
    // No store, no .objects/, nothing on disk — just data structures.
    let set = prune_set(&m, &dest, &[]);
    assert_eq!(
        set,
        vec!["./b.txt".to_string()],
        "prune-set must be derivable purely from path+type listings; got {set:?}"
    );
}

#[test]
fn prune_set_ignores_checksum_field_entirely() {
    // SPEC: content is never consulted. Two manifests differing ONLY by the
    // (bogus) checksum of an entry yield the identical prune-set, proving the
    // checksum field is not part of the keep/prune decision.
    let dest = vec![dd("./"), df("./keep"), df("./drop")];

    let m1 = manifest(&[md("./"), mf("./keep")]);
    let weird = "F 600 0000000000000000000000000000000000000000000000000000000000000000 999 ./keep";
    let m2 = Manifest::parse(&format!("{}\n{weird}", md("./"))).expect("parses");

    let s1 = prune_set(&m1, &dest, &[]);
    let s2 = prune_set(&m2, &dest, &[]);
    assert_eq!(
        s1, s2,
        "prune-set must not depend on checksum/size; {s1:?} vs {s2:?}"
    );
    assert_eq!(s1, vec!["./drop".to_string()]);
}

// ===========================================================================
// FILE <-> DIR TYPE CHANGE: replaced, not kept (both directions)
// ===========================================================================

#[test]
fn manifest_file_but_dest_dir_is_extraneous_replace_not_keep() {
    // SPEC: "if the manifest has path `p` as a File but the dest has `p` as a
    // Directory ... `p` (the dest form) is extraneous and must be in the
    // prune-set (so materialize can replace it with the correct type)."
    // Manifest: p is a File. Dest: p/ is a Directory (with a child).
    let m = manifest(&[md("./"), mf("./p")]);
    let dest = vec![dd("./"), dd("./p/"), df("./p/child")];
    let set = prune_set(&m, &dest, &[]);
    // The dest directory ./p/ (and its child) must be pruned: the manifest
    // wants a File at p, so the directory form is extraneous.
    assert!(
        set.contains(&"./p/".to_string()),
        "dest dir ./p/ where manifest has file ./p must be pruned (type change); got {set:?}"
    );
    assert!(
        set.contains(&"./p/child".to_string()),
        "the child under the to-be-replaced dir must also be pruned; got {set:?}"
    );
}

#[test]
fn manifest_dir_but_dest_file_is_extraneous_replace_not_keep() {
    // SPEC (other direction): manifest has `p` as a Directory, dest has `p` as
    // a File => the dest file is extraneous (must be replaced by the dir).
    let m = manifest(&[md("./"), md("./p/"), mf("./p/inside")]);
    let dest = vec![dd("./"), df("./p")];
    let set = prune_set(&m, &dest, &[]);
    assert!(
        set.contains(&"./p".to_string()),
        "dest file ./p where manifest has dir ./p/ must be pruned (type change); got {set:?}"
    );
}

#[test]
fn type_change_pruned_even_when_path_string_matches_modulo_trailing_slash() {
    // SPEC corollary: the dest dir entry (`./p/`) and the manifest file entry
    // (`./p`) are NOT the same path key — the trailing slash distinguishes
    // type. The dir form is extraneous; nothing in the manifest "saves" it.
    let m = manifest(&[md("./"), mf("./p")]);
    let dest = vec![dd("./"), dd("./p/")];
    let set = prune_set(&m, &dest, &[]);
    assert_eq!(
        set,
        vec!["./p/".to_string()],
        "the dir form ./p/ is extraneous against a file ./p; got {set:?}"
    );
}

// ===========================================================================
// DEEPEST-FIRST ORDERING for nested extraneous trees
// ===========================================================================

#[test]
fn nested_extraneous_tree_is_ordered_deepest_first() {
    // SPEC: "Nested directories must be ordered deepest-first ... a nested
    // extraneous tree yields a deletion order with descendants before their
    // ancestors." Manifest is just the root; the whole ./junk/ subtree is extra.
    let m = manifest(&[md("./")]);
    let dest = vec![
        dd("./"),
        dd("./junk/"),
        dd("./junk/deep/"),
        df("./junk/deep/leaf.txt"),
        df("./junk/top.txt"),
    ];
    let set = prune_set(&m, &dest, &[]);
    assert!(
        is_deepest_first(&set),
        "removal order must unlink descendants before ancestors; got {set:?}"
    );
    // Concretely: the deepest leaf precedes ./junk/deep/ which precedes ./junk/.
    let pos = |p: &str| set.iter().position(|x| x == p).expect("present in set");
    assert!(pos("./junk/deep/leaf.txt") < pos("./junk/deep/"));
    assert!(pos("./junk/deep/") < pos("./junk/"));
    assert!(pos("./junk/top.txt") < pos("./junk/"));
}

#[test]
fn sibling_subtrees_each_internally_deepest_first() {
    // SPEC reinforcement: multiple independent extraneous subtrees each obey
    // deepest-first internally (cross-subtree order is unconstrained, so we
    // only assert the ancestor/descendant invariant via the helper).
    let m = manifest(&[md("./")]);
    let dest = vec![
        dd("./"),
        dd("./x/"),
        df("./x/x1"),
        dd("./y/"),
        dd("./y/yy/"),
        df("./y/yy/y2"),
    ];
    let set = prune_set(&m, &dest, &[]);
    assert!(
        is_deepest_first(&set),
        "each subtree must be deepest-first; got {set:?}"
    );
}

// ===========================================================================
// EMPTY EXTRANEOUS DIRECTORIES are included
// ===========================================================================

#[test]
fn empty_extraneous_directory_is_included() {
    // SPEC: "Empty extraneous directories are included in the prune-set."
    let m = manifest(&[md("./"), mf("./keep")]);
    let dest = vec![dd("./"), df("./keep"), dd("./emptydir/")];
    let set = prune_set(&m, &dest, &[]);
    assert!(
        set.contains(&"./emptydir/".to_string()),
        "an empty extraneous directory must be pruned; got {set:?}"
    );
}

#[test]
fn nested_empty_extraneous_dirs_ordered_deepest_first() {
    // SPEC: empty dirs included AND deepest-first. A chain of empty dirs with
    // no files at the bottom must still order child before parent.
    let m = manifest(&[md("./")]);
    let dest = vec![dd("./"), dd("./a/"), dd("./a/b/"), dd("./a/b/c/")];
    let set = prune_set(&m, &dest, &[]);
    assert!(
        is_deepest_first(&set),
        "empty-dir chain must be deepest-first; got {set:?}"
    );
    let pos = |p: &str| set.iter().position(|x| x == p).expect("present");
    assert!(pos("./a/b/c/") < pos("./a/b/"));
    assert!(pos("./a/b/") < pos("./a/"));
}

// ===========================================================================
// EXCLUDE: matching extraneous paths are PROTECTED (opt-out)
// ===========================================================================

#[test]
fn exclude_protects_matching_extraneous_path() {
    // SPEC: "an extraneous path that matches an exclude pattern is PROTECTED,
    // i.e. NOT pruned." `./protected.log` would be extraneous but is excluded;
    // `./pruned.tmp` is extraneous and NOT excluded => still pruned.
    let m = manifest(&[md("./"), mf("./keep")]);
    let dest = vec![
        dd("./"),
        df("./keep"),
        df("./protected.log"),
        df("./pruned.tmp"),
    ];
    let set = prune_set(&m, &dest, &["protected.log"]);
    assert!(
        !set.contains(&"./protected.log".to_string()),
        "an excluded extraneous path must be protected (not pruned); got {set:?}"
    );
    assert!(
        set.contains(&"./pruned.tmp".to_string()),
        "a non-excluded extraneous path must still be pruned; got {set:?}"
    );
}

#[test]
fn exclude_does_not_resurrect_a_manifest_path() {
    // SPEC corollary: excludes only REMOVE from the prune-set; a kept (in
    // manifest) path was never in the set, so an exclude that matches it is a
    // no-op — the set is unchanged.
    let m = manifest(&[md("./"), mf("./keep")]);
    let dest = vec![dd("./"), df("./keep"), df("./drop")];
    let with = prune_set(&m, &dest, &["keep"]);
    let without = prune_set(&m, &dest, &[]);
    assert_eq!(
        with, without,
        "excluding a path that is already kept changes nothing; {with:?} vs {without:?}"
    );
    assert_eq!(with, vec!["./drop".to_string()]);
}

#[test]
fn multiple_excludes_all_apply() {
    // SPEC: each --exclude pattern protects matching extraneous paths. Two
    // patterns protect two different files; a third unmatched file is pruned.
    let m = manifest(&[md("./")]);
    let dest = vec![
        dd("./"),
        df("./a.keepme"),
        df("./b.keepme2"),
        df("./c.gone"),
    ];
    let set = prune_set(&m, &dest, &["keepme", "keepme2"]);
    assert!(!set.contains(&"./a.keepme".to_string()), "got {set:?}");
    assert!(!set.contains(&"./b.keepme2".to_string()), "got {set:?}");
    assert!(set.contains(&"./c.gone".to_string()), "got {set:?}");
}

#[test]
fn exclude_protecting_dir_protects_whole_subtree_paths_that_match() {
    // SPEC: excludes operate on each path; a pattern matching a directory
    // segment protects every dest path under it that the regex also matches.
    // Using the segment fragment "node_modules" (matches the dir and its
    // children, since the regex matches anywhere in the path).
    let m = manifest(&[md("./")]);
    let dest = vec![
        dd("./"),
        dd("./node_modules/"),
        df("./node_modules/pkg/index.js"),
        df("./build.tmp"),
    ];
    let set = prune_set(&m, &dest, &["node_modules"]);
    assert!(
        !set.iter().any(|p| p.contains("node_modules")),
        "every node_modules path must be protected; got {set:?}"
    );
    assert!(
        set.contains(&"./build.tmp".to_string()),
        "the non-matching extraneous file must still be pruned; got {set:?}"
    );
}

// ===========================================================================
// IDEMPOTENCY / DEGENERATE INPUTS
// ===========================================================================

#[test]
fn dest_exactly_equals_manifest_yields_empty_prune_set() {
    // SPEC: "dest exactly equals manifest -> empty prune-set."
    let m = manifest(&[md("./"), md("./a/"), mf("./a/f"), mf("./r")]);
    let dest = vec![dd("./"), dd("./a/"), df("./a/f"), df("./r")];
    let set = prune_set(&m, &dest, &[]);
    assert!(
        set.is_empty(),
        "exact mirror must produce no deletions; got {set:?}"
    );
}

#[test]
fn empty_dest_yields_empty_prune_set() {
    // SPEC: "dest empty -> empty prune-set." Nothing on disk => nothing to remove.
    let m = manifest(&[md("./"), mf("./a"), mf("./b")]);
    let set = prune_set(&m, &[], &[]);
    assert!(
        set.is_empty(),
        "an empty dest can have nothing extraneous; got {set:?}"
    );
}

#[test]
fn empty_manifest_with_nonempty_dest_prunes_everything_except_root() {
    // SPEC: "manifest empty + dest non-empty -> all dest paths extraneous."
    // The root ./ is special: an empty manifest still implies the dest root
    // exists as the mirror target. We assert all NON-root dest paths are
    // extraneous; whether the bare root is itself listed is asserted by the
    // root-specific test below. Here we use a manifest with only the root.
    let m = manifest(&[md("./")]);
    let dest = vec![dd("./"), df("./a"), dd("./d/"), df("./d/e")];
    let set = prune_set(&m, &dest, &[]);
    assert!(set.contains(&"./a".to_string()), "got {set:?}");
    assert!(set.contains(&"./d/".to_string()), "got {set:?}");
    assert!(set.contains(&"./d/e".to_string()), "got {set:?}");
    assert!(
        !set.contains(&"./".to_string()),
        "the mirror root ./ (present in manifest) must never be pruned; got {set:?}"
    );
    assert!(is_deepest_first(&set), "got {set:?}");
}

#[test]
fn totally_empty_manifest_and_empty_dest_is_empty() {
    // SPEC degenerate corner: both empty => empty.
    let m = Manifest::new();
    let set = prune_set(&m, &[], &[]);
    assert!(set.is_empty(), "nothing in, nothing out; got {set:?}");
}

#[test]
fn idempotent_across_repeated_calls() {
    // SPEC: idempotency / re-runs. Calling twice on the same inputs yields the
    // identical ordered result (no hidden state, stable order).
    let m = manifest(&[md("./"), mf("./keep")]);
    let dest = vec![dd("./"), df("./keep"), dd("./x/"), df("./x/y"), df("./z")];
    let a = prune_set(&m, &dest, &[]);
    let b = prune_set(&m, &dest, &[]);
    assert_eq!(a, b, "prune-set must be deterministic and order-stable");
}

#[test]
fn manifest_directory_also_in_dest_is_kept() {
    // SPEC: "Directories in the manifest that also exist in the dest are kept."
    let m = manifest(&[md("./"), md("./shared/"), mf("./shared/f")]);
    let dest = vec![dd("./"), dd("./shared/"), df("./shared/f")];
    let set = prune_set(&m, &dest, &[]);
    assert!(
        !set.contains(&"./shared/".to_string()),
        "a directory present in both manifest and dest must be kept; got {set:?}"
    );
    assert!(set.is_empty(), "got {set:?}");
}

// ===========================================================================
// UNICODE / SPACE / DOT-PREFIXED PATHS
// ===========================================================================

#[test]
fn unicode_and_space_and_dot_prefixed_extraneous_paths_handled() {
    // SPEC: "unicode / space / dot-prefixed paths handled." Each extraneous
    // path with awkward bytes must still be detected as extraneous and kept
    // verbatim (no normalization) in the output.
    let m = manifest(&[md("./"), mf("./keep")]);
    let dest = vec![
        dd("./"),
        df("./keep"),
        df("./naïve café.txt"),     // unicode + space
        df("./a file with spaces"), // spaces
        df("./.hidden"),            // dot-prefixed (not the ./ prefix)
        df("./日本語.txt"),         // unicode file name
    ];
    let set = prune_set(&m, &dest, &[]);
    assert!(set.contains(&"./naïve café.txt".to_string()), "got {set:?}");
    assert!(
        set.contains(&"./a file with spaces".to_string()),
        "got {set:?}"
    );
    assert!(set.contains(&"./.hidden".to_string()), "got {set:?}");
    assert!(set.contains(&"./日本語.txt".to_string()), "got {set:?}");
}

#[test]
fn dot_prefixed_file_present_in_manifest_is_kept() {
    // SPEC: dot-prefixed paths are ordinary paths — one in the manifest is kept.
    let m = manifest(&[md("./"), mf("./.config")]);
    let dest = vec![dd("./"), df("./.config"), df("./.junk")];
    let set = prune_set(&m, &dest, &[]);
    assert!(
        !set.contains(&"./.config".to_string()),
        "dotfile in manifest kept; got {set:?}"
    );
    assert!(
        set.contains(&"./.junk".to_string()),
        "extraneous dotfile pruned; got {set:?}"
    );
}

#[test]
fn space_bearing_path_is_keyed_verbatim_not_split() {
    // SPEC corollary + manifest-format note: the path field may contain spaces
    // and is keyed verbatim. A space-bearing path present in the manifest is
    // kept; only the truly-extraneous space-bearing path is pruned.
    let m = manifest(&[md("./"), mf("./keep me.txt")]);
    let dest = vec![dd("./"), df("./keep me.txt"), df("./drop me.txt")];
    let set = prune_set(&m, &dest, &[]);
    assert!(!set.contains(&"./keep me.txt".to_string()), "got {set:?}");
    assert!(set.contains(&"./drop me.txt".to_string()), "got {set:?}");
}

// ===========================================================================
// MIXED REALISTIC SCENARIO (everything at once)
// ===========================================================================

#[test]
fn mixed_scenario_keep_replace_prune_exclude_ordered() {
    // SPEC integration: a single call exercising keep, type-change-replace,
    // nested-extraneous deepest-first, empty-dir, and exclude-protect together.
    let m = manifest(&[
        md("./"),
        mf("./keep.txt"), // present in dest, same type => kept
        md("./libdir/"),  // present in dest as dir => kept
        mf("./libdir/lib.rs"),
        mf("./becomes_file"), // manifest says File; dest has it as a dir => replace
    ]);
    let dest = vec![
        dd("./"),
        df("./keep.txt"),
        dd("./libdir/"),
        df("./libdir/lib.rs"),
        dd("./becomes_file/"),      // type change -> prune the dir form
        df("./becomes_file/stale"), // its child -> prune
        dd("./trash/"),             // wholly extraneous subtree
        dd("./trash/sub/"),
        df("./trash/sub/x"),
        dd("./emptyextra/"),    // empty extraneous dir
        df("./protected.keep"), // extraneous but excluded => protected
    ];
    let set = prune_set(&m, &dest, &["protected.keep"]);

    // Kept paths absent from the prune-set.
    for kept in [
        "./",
        "./keep.txt",
        "./libdir/",
        "./libdir/lib.rs",
        "./protected.keep",
    ] {
        assert!(
            !set.contains(&kept.to_string()),
            "{kept} must be kept; got {set:?}"
        );
    }
    // Pruned paths present.
    for pruned in [
        "./becomes_file/",
        "./becomes_file/stale",
        "./trash/",
        "./trash/sub/",
        "./trash/sub/x",
        "./emptyextra/",
    ] {
        assert!(
            set.contains(&pruned.to_string()),
            "{pruned} must be pruned; got {set:?}"
        );
    }
    // Ordering invariant holds across the whole set.
    assert!(
        is_deepest_first(&set),
        "whole-set deletion order must be deepest-first; got {set:?}"
    );
}
