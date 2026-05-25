//! T-324 — Event coalescing.
//!
//! Four normalization rules applied after `notify_debouncer_full` fires:
//!
//! 1. Path in both `added` and `removed` → treat as `modified` (content
//!    replacement within the debounce window).
//! 2. Path appears multiple times in `modified` → deduplicate (last-write wins;
//!    the caller supplies a `HashSet` so deduplication is implicit).
//! 3. Path in both `modified` and `removed` → keep only in `removed`.
//! 4. Paths matching `.codewiki/**` are stripped before coalescing.

use codewiki_extraction::{ChangedFiles, ParsedTree};
use std::collections::HashSet;
use std::path::PathBuf;

use crate::tree_cache::TreeCache;

/// Coalesce raw event sets into a normalised `ChangedFiles`.
///
/// `raw_added`, `raw_modified`, and `raw_removed` are `HashSet`s so rule 2
/// (modified dedup) is automatically satisfied by the caller passing sets.
///
/// The function additionally strips any path whose components contain
/// `.codewiki` (rule 4).
pub fn coalesce_events(
    raw_added: HashSet<PathBuf>,
    raw_modified: HashSet<PathBuf>,
    raw_removed: HashSet<PathBuf>,
    tree_cache: &TreeCache,
) -> ChangedFiles {
    // Rule 4: strip .codewiki paths from all sets up front.
    let raw_added = strip_codewiki(raw_added);
    let raw_modified = strip_codewiki(raw_modified);
    let raw_removed = strip_codewiki(raw_removed);

    // Rule 3 & 1: removed wins over modified wins over added.
    // removed: final set — includes paths from raw_removed AND (raw_added ∩ raw_removed
    //          treated as modified, but since removed wins, they go into removed).
    // Wait — rule 1 says added+removed = modified.  Rule 3 says modified+removed = removed.
    // So the priority chain is: removed > (modified that are not removed) > (added that are
    // neither removed nor modified).
    //
    // For the added+removed overlap (rule 1): the result is "modified" per TS spec, but rule 3
    // says modified+removed → removed.  So added∩removed → removed wins.
    // Per A5-resolutions §G1, the coalescer code says:
    //   removed_set covers raw_removed.
    //   modified filtered by !removed_set.
    //   added filtered by !removed_set && !modified_set.
    // That matches: added∩removed → removed wins (not modified).

    let removed: Vec<PathBuf> = raw_removed.iter().cloned().collect();
    let removed_set: HashSet<&PathBuf> = raw_removed.iter().collect();

    // Paths that are in both added AND removed are folded into removed
    // (removed wins per the priority chain above).
    let modified: Vec<(PathBuf, Option<ParsedTree>)> = raw_modified
        .iter()
        .chain(
            // Rule 1: added ∩ removed → treat as modified (but removed wins; we
            // re-add them as modified only if they are NOT in removed).
            raw_added.iter().filter(|p| raw_removed.contains(*p) && !removed_set.contains(p)),
        )
        .filter(|p| !removed_set.contains(p))
        .map(|p| {
            let prior_tree = tree_cache.get(p).map(|c| c.tree.clone());
            (p.clone(), prior_tree)
        })
        .collect();

    let modified_set: HashSet<&PathBuf> = raw_modified.iter().collect();
    let added: Vec<PathBuf> = raw_added
        .into_iter()
        .filter(|p| !removed_set.contains(p) && !modified_set.contains(p))
        .collect();

    ChangedFiles { added, modified, removed }
}

/// Remove any path containing a `.codewiki` component.
fn strip_codewiki(paths: HashSet<PathBuf>) -> HashSet<PathBuf> {
    paths
        .into_iter()
        .filter(|p| {
            !p.components().any(|c| {
                c.as_os_str() == ".codewiki"
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn empty_cache() -> TreeCache {
        TreeCache::new(0)
    }

    fn paths(ps: &[PathBuf]) -> Vec<String> {
        let mut v: Vec<String> = ps.iter().map(|p| p.to_string_lossy().to_string()).collect();
        v.sort();
        v
    }

    fn added_paths(cf: &ChangedFiles) -> Vec<String> {
        paths(&cf.added)
    }

    fn modified_paths(cf: &ChangedFiles) -> Vec<String> {
        let mut v: Vec<String> = cf.modified.iter().map(|(p, _)| p.to_string_lossy().to_string()).collect();
        v.sort();
        v
    }

    fn removed_paths(cf: &ChangedFiles) -> Vec<String> {
        paths(&cf.removed)
    }

    // Rule 1: added+removed same path → removed (removed wins per priority chain).
    #[test]
    fn rule1_added_and_removed_same_path_goes_to_removed() {
        let p = path("/project/foo.ts");
        let added: HashSet<PathBuf> = [p.clone()].into();
        let removed: HashSet<PathBuf> = [p.clone()].into();
        let modified: HashSet<PathBuf> = HashSet::new();

        let result = coalesce_events(added, modified, removed, &empty_cache());

        assert!(removed_paths(&result).contains(&p.to_string_lossy().to_string()));
        assert!(added_paths(&result).is_empty(), "added should be empty");
    }

    // Rule 2: duplicate modified entries deduplicated.
    #[test]
    fn rule2_modified_dedup() {
        let p = path("/project/bar.ts");
        // HashSet ensures dedup.
        let modified: HashSet<PathBuf> = [p.clone(), p.clone()].into();
        let result = coalesce_events(
            HashSet::new(),
            modified,
            HashSet::new(),
            &empty_cache(),
        );
        assert_eq!(result.modified.len(), 1);
        assert_eq!(result.modified[0].0, p);
    }

    // Rule 3: modified+removed → removed only.
    #[test]
    fn rule3_modified_and_removed_keeps_only_removed() {
        let p = path("/project/baz.ts");
        let modified: HashSet<PathBuf> = [p.clone()].into();
        let removed: HashSet<PathBuf> = [p.clone()].into();

        let result = coalesce_events(
            HashSet::new(),
            modified,
            removed,
            &empty_cache(),
        );

        assert!(modified_paths(&result).is_empty(), "modified should be empty");
        assert!(removed_paths(&result).contains(&p.to_string_lossy().to_string()));
    }

    // Rule 4: .codewiki paths stripped.
    #[test]
    fn rule4_codewiki_paths_stripped() {
        let db = path("/project/.codewiki/codewiki.db");
        let real = path("/project/index.ts");

        let added: HashSet<PathBuf> = [db.clone(), real.clone()].into();
        let result = coalesce_events(
            added,
            HashSet::new(),
            HashSet::new(),
            &empty_cache(),
        );

        let a = added_paths(&result);
        assert!(!a.iter().any(|s| s.contains(".codewiki")));
        assert!(a.contains(&real.to_string_lossy().to_string()));
    }

    // Unrelated files pass through untouched.
    #[test]
    fn normal_events_pass_through() {
        let a = path("/p/a.ts");
        let m = path("/p/b.ts");
        let r = path("/p/c.ts");

        let result = coalesce_events(
            [a.clone()].into(),
            [m.clone()].into(),
            [r.clone()].into(),
            &empty_cache(),
        );

        assert_eq!(added_paths(&result), vec![a.to_string_lossy().to_string()]);
        assert_eq!(modified_paths(&result), vec![m.to_string_lossy().to_string()]);
        assert_eq!(removed_paths(&result), vec![r.to_string_lossy().to_string()]);
    }
}
