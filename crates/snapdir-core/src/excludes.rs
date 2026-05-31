//! Exclude-pattern expansion and matching, plus the follow/no-follow setting.
//!
//! The oracle (`snapdir-manifest`) applies excludes as an **extended regular
//! expression** fed to `grep -E -v`: a path is excluded when the regex matches
//! it. The user-supplied `--exclude` pattern may embed two macros that expand
//! to built-in sets, lifted verbatim from `_snapdir_manifest_define_exclude_patterns`:
//!
//! - `%system%` expands to the system directory set and **forces `--no-follow`**.
//! - `%common%` expands to the common directory set (`.git`, `.cache`,
//!   `node_modules`, `.DS_Store`, Trash dirs, …).
//!
//! Per the library-purity principle, `snapdir-core` reads **no** environment.
//! The oracle's `%system%` set interpolates two runtime paths — `${HOME}/.cache/`
//! and the resolved cache directory `${_SNAPDIR_MANIFEST_CACHE_DIR}` — so those
//! are passed in as parameters; the CLI lane resolves `$HOME` / `XDG_CACHE_HOME`
//! and hands them to [`expand_excludes`]. The built-in literal sets themselves
//! match the oracle's hard-coded defaults (when `SNAPDIR_SYSTEM_EXCLUDE_DIRS` /
//! `SNAPDIR_COMMON_EXCLUDE_DIRS` are unset).
//!
//! The filesystem walk that actually consults [`ExcludeMatcher::is_excluded`]
//! lands in a later gate; this module models the expansion + matcher + the
//! follow/no-follow option semantics, validated against the Bash source.

use regex::Regex;
use thiserror::Error;

/// The oracle's default system exclude directory list — the body of
/// `SNAPDIR_SYSTEM_EXCLUDE_DIRS`'s default (the leading-`^`-anchored set,
/// excluding the trailing `${HOME}/.cache/` and cache-dir entries, which are
/// runtime values appended in [`expand_excludes`]).
///
/// Copied verbatim from `_snapdir_manifest_define_exclude_patterns`.
pub const SYSTEM_EXCLUDE_DIRS: &str = "/vscode/|/dev/|/proc/|/sys/|/tmp/|/var/run/|/run/|/mnt/|/media/|/lost+found/|/var/snap/lxd/common/ns/shmounts/|/var/snap/lxd/common/ns/mntns/|/var/lib/lxcfs/";

/// The oracle's default common exclude directory list — the body of
/// `SNAPDIR_COMMON_EXCLUDE_DIRS`'s default.
///
/// Copied verbatim from `_snapdir_manifest_define_exclude_patterns`.
pub const COMMON_EXCLUDE_DIRS: &str = ".cache|.git|.DS_Store|.vscode-server|.dbus|.gvfs|.local/share/gvfs-metadata|.local/share/Trash|.Trash|node_modules|Trash-1000";

/// Whether the filesystem walk follows symbolic links.
///
/// Mirrors the oracle's `_snapdir_manifest_find_flags`: the default is
/// [`Follow`](FollowMode::Follow) (`find -L`), and `--no-follow` (or a
/// `%system%` expansion) switches to [`NoFollow`](FollowMode::NoFollow)
/// (plain `find`, dropping symlinks).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FollowMode {
    /// Follow symlinks (the default; `find -L`).
    #[default]
    Follow,
    /// Do not follow symlinks (`--no-follow`; plain `find`).
    NoFollow,
}

impl FollowMode {
    /// Returns `true` if symlinks are followed.
    #[must_use]
    pub fn follows_symlinks(self) -> bool {
        matches!(self, Self::Follow)
    }
}

/// Errors raised while expanding/compiling an exclude pattern.
#[derive(Debug, Error)]
pub enum ExcludeError {
    /// The expanded pattern was not a valid extended regular expression.
    #[error("invalid exclude regex: {0}")]
    InvalidRegex(#[from] regex::Error),
}

/// The result of expanding a `--exclude` pattern: the final extended-regex
/// string plus whether the expansion forced `--no-follow`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpandedExclude {
    /// The final extended-regex pattern (with `%system%` / `%common%`
    /// substituted), or `None` when the input was empty (no exclusion).
    pub pattern: Option<String>,
    /// `true` when `%system%` appeared and forced no-follow.
    pub forces_no_follow: bool,
}

/// Expands the `%system%` / `%common%` macros in a `--exclude` pattern.
///
/// Reproduces `_snapdir_manifest_define_exclude_patterns` exactly:
///
/// - every occurrence of the literal `%system%` is replaced with
///   `(^(<SYSTEM_EXCLUDE_DIRS>|<home_cache>|<cache_dir>))` and `forces_no_follow`
///   is set;
/// - every occurrence of the literal `%common%` is replaced with
///   `(/(<COMMON_EXCLUDE_DIRS>)($|/))`.
///
/// `home_cache` is `${HOME}/.cache/` and `cache_dir` is the resolved
/// `_SNAPDIR_MANIFEST_CACHE_DIR`; both are runtime values the CLI lane resolves
/// and passes in (core reads no environment). An empty `pattern` yields no
/// exclusion (matching the oracle, which only filters when the pattern is
/// non-empty).
#[must_use]
pub fn expand_excludes(pattern: &str, home_cache: &str, cache_dir: &str) -> ExpandedExclude {
    if pattern.is_empty() {
        return ExpandedExclude {
            pattern: None,
            forces_no_follow: false,
        };
    }

    let mut expanded = pattern.to_owned();
    let mut forces_no_follow = false;

    if expanded.contains("%system%") {
        let system_set = format!("(^({SYSTEM_EXCLUDE_DIRS}|{home_cache}|{cache_dir}))");
        expanded = expanded.replace("%system%", &system_set);
        forces_no_follow = true;
    }
    if expanded.contains("%common%") {
        let common_set = format!("(/({COMMON_EXCLUDE_DIRS})($|/))");
        expanded = expanded.replace("%common%", &common_set);
    }

    ExpandedExclude {
        pattern: Some(expanded),
        forces_no_follow,
    }
}

/// A compiled exclude matcher: a path is excluded when the (extended) regex
/// matches anywhere in it, mirroring `grep -E -v`.
#[derive(Debug, Clone)]
pub struct ExcludeMatcher {
    regex: Regex,
}

impl ExcludeMatcher {
    /// Compiles an already-expanded extended-regex exclude pattern.
    ///
    /// # Errors
    ///
    /// Returns [`ExcludeError::InvalidRegex`] if `pattern` is not a valid
    /// extended regular expression.
    pub fn new(pattern: &str) -> Result<Self, ExcludeError> {
        Ok(Self {
            regex: Regex::new(pattern)?,
        })
    }

    /// Returns `true` when `path` is excluded (the regex matches anywhere in
    /// it), matching `grep -E -v`'s "drop matching lines" semantics.
    #[must_use]
    pub fn is_excluded(&self, path: &str) -> bool {
        self.regex.is_match(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Representative runtime values the CLI lane would resolve.
    const HOME_CACHE: &str = "/home/user/.cache/";
    const CACHE_DIR: &str = "/home/user/.cache/snapdir";

    #[test]
    fn exclude_system_expands_to_oracle_set_and_forces_no_follow() {
        let out = expand_excludes("%system%", HOME_CACHE, CACHE_DIR);
        let expected = format!("(^({SYSTEM_EXCLUDE_DIRS}|{HOME_CACHE}|{CACHE_DIR}))");
        assert_eq!(out.pattern.as_deref(), Some(expected.as_str()));
        assert!(out.forces_no_follow, "%system% must force no-follow");
    }

    #[test]
    fn exclude_common_expands_to_oracle_set_without_forcing_no_follow() {
        let out = expand_excludes("%common%", HOME_CACHE, CACHE_DIR);
        let expected = format!("(/({COMMON_EXCLUDE_DIRS})($|/))");
        assert_eq!(out.pattern.as_deref(), Some(expected.as_str()));
        assert!(
            !out.forces_no_follow,
            "%common% alone must NOT force no-follow"
        );
    }

    #[test]
    fn exclude_combines_user_pattern_with_both_macros() {
        // The oracle substitutes in place, leaving the user's literal alongside
        // the expanded sets joined by the regex alternation the user wrote.
        let out = expand_excludes(".ignore|%common%|%system%", HOME_CACHE, CACHE_DIR);
        let pattern = out.pattern.expect("non-empty");
        assert!(pattern.starts_with(".ignore|"));
        assert!(pattern.contains("node_modules"));
        assert!(pattern.contains("/proc/"));
        assert!(out.forces_no_follow, "%system% present forces no-follow");
    }

    #[test]
    fn exclude_empty_pattern_yields_no_exclusion() {
        let out = expand_excludes("", HOME_CACHE, CACHE_DIR);
        assert_eq!(out.pattern, None);
        assert!(!out.forces_no_follow);
    }

    #[test]
    fn exclude_user_pattern_passes_through_verbatim() {
        // A plain user regex with no macros is used as-is.
        let out = expand_excludes(".git|.DS_Store", HOME_CACHE, CACHE_DIR);
        assert_eq!(out.pattern.as_deref(), Some(".git|.DS_Store"));
        assert!(!out.forces_no_follow);
    }

    #[test]
    fn exclude_matcher_matches_representative_common_paths() {
        let out = expand_excludes("%common%", HOME_CACHE, CACHE_DIR);
        let matcher = ExcludeMatcher::new(&out.pattern.unwrap()).expect("valid regex");

        // The common set is anchored `(/(...)($|/))`: a `/.git` segment that
        // ends the path or is followed by `/` matches.
        assert!(matcher.is_excluded("/project/.git/config"));
        assert!(matcher.is_excluded("/project/node_modules/pkg/index.js"));
        assert!(matcher.is_excluded("/home/user/.DS_Store"));
        assert!(matcher.is_excluded("/repo/.cache"));

        // Non-matching: no excluded segment.
        assert!(!matcher.is_excluded("/project/src/main.rs"));
        assert!(!matcher.is_excluded("/project/readme.md"));
        // `.gitignore` is NOT `.git` as a path segment, so it must NOT match.
        assert!(!matcher.is_excluded("/project/.gitignore"));
    }

    #[test]
    fn exclude_matcher_matches_representative_system_paths() {
        let out = expand_excludes("%system%", HOME_CACHE, CACHE_DIR);
        let matcher = ExcludeMatcher::new(&out.pattern.unwrap()).expect("valid regex");

        // The system set is anchored at start-of-path `(^(...))`.
        assert!(matcher.is_excluded("/proc/cpuinfo"));
        assert!(matcher.is_excluded("/dev/null"));
        assert!(matcher.is_excluded("/sys/kernel"));
        assert!(matcher.is_excluded("/tmp/scratch"));
        assert!(matcher.is_excluded("/home/user/.cache/thing"));

        // Anchored at start: a `/proc/` appearing mid-path does NOT match.
        assert!(!matcher.is_excluded("/data/proc/file"));
        assert!(!matcher.is_excluded("/home/user/project/main.rs"));
    }

    #[test]
    fn exclude_matcher_user_regex_is_extended_regex() {
        // grep -E semantics: alternation without backslashes.
        let matcher = ExcludeMatcher::new("foo|bar").expect("valid regex");
        assert!(matcher.is_excluded("/a/foo/b"));
        assert!(matcher.is_excluded("/x/bar"));
        assert!(!matcher.is_excluded("/x/baz"));
    }

    // --- follow / no-follow option semantics ------------------------------

    #[test]
    fn no_follow_default_is_follow() {
        assert_eq!(FollowMode::default(), FollowMode::Follow);
        assert!(FollowMode::default().follows_symlinks());
    }

    #[test]
    fn no_follow_drops_symlinks() {
        assert!(!FollowMode::NoFollow.follows_symlinks());
        assert!(FollowMode::Follow.follows_symlinks());
    }

    #[test]
    fn no_follow_forced_by_system_exclude() {
        // The %system% macro forces no-follow; the resolved FollowMode must
        // flip to NoFollow even if the caller started from the Follow default.
        let out = expand_excludes("%system%", HOME_CACHE, CACHE_DIR);
        let mode = if out.forces_no_follow {
            FollowMode::NoFollow
        } else {
            FollowMode::Follow
        };
        assert_eq!(mode, FollowMode::NoFollow);
        assert!(!mode.follows_symlinks());
    }

    #[test]
    fn no_follow_not_forced_by_common_or_plain_exclude() {
        // %common% and plain user patterns leave the follow setting untouched.
        for pat in ["%common%", ".git", ""] {
            let out = expand_excludes(pat, HOME_CACHE, CACHE_DIR);
            assert!(
                !out.forces_no_follow,
                "pattern {pat:?} must not force no-follow"
            );
        }
    }
}
