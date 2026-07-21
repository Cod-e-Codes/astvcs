//! Hash-anchor sibling alignment and bounded LCS for small remainders.

use std::collections::HashMap;
use std::hash::Hash;

use crate::diff::lcs::lcs_pairs;

/// Run full LCS only when the unmatched cross-product is at or below this size.
/// Wide sibling lists skip LCS and rely on hash anchors plus structural fallbacks.
pub const LCS_THRESHOLD: usize = 48;

fn index_buckets<T: Eq + Hash + Clone>(slice: &[T]) -> HashMap<T, Vec<usize>> {
    let mut map: HashMap<T, Vec<usize>> = HashMap::new();
    for (i, value) in slice.iter().enumerate() {
        map.entry(value.clone()).or_default().push(i);
    }
    map
}

/// Pair indices where the same value appears exactly once on each side.
pub fn pair_unique_bijective<T: Eq + Hash + Clone>(old: &[T], new: &[T]) -> Vec<(usize, usize)> {
    let old_buckets = index_buckets(old);
    let new_buckets = index_buckets(new);
    let mut pairs = Vec::new();
    for (value, old_indices) in &old_buckets {
        if let Some(new_indices) = new_buckets.get(value)
            && old_indices.len() == 1
            && new_indices.len() == 1
        {
            pairs.push((old_indices[0], new_indices[0]));
        }
    }
    pairs
}

/// Pair indices sharing the same key, matching in ascending index order within each bucket.
pub fn pair_in_order_by_key<T: Eq + Hash + Clone>(old: &[T], new: &[T]) -> Vec<(usize, usize)> {
    let old_buckets = index_buckets(old);
    let new_buckets = index_buckets(new);
    let mut pairs = Vec::new();
    for (key, old_indices) in &old_buckets {
        if let Some(new_indices) = new_buckets.get(key) {
            for (oi, ni) in old_indices.iter().zip(new_indices.iter()) {
                pairs.push((*oi, *ni));
            }
        }
    }
    pairs
}

/// Pair old and new indices sharing the same `NodeId`, matching duplicates in list order.
/// Content-addressed ids can repeat among siblings (e.g. two `,` tokens).
/// Wide sibling lists use `lcs_pairs` on structural `(kind, payload)` keys per child.
/// remains for unit tests and narrow remainders.
#[allow(dead_code)]
pub fn pair_equal_node_ids<T: Eq + Hash + Clone>(old: &[T], new: &[T]) -> Vec<(usize, usize)> {
    let old_buckets = index_buckets(old);
    let new_buckets = index_buckets(new);
    let mut pairs = Vec::new();
    for (id, old_indices) in &old_buckets {
        if let Some(new_indices) = new_buckets.get(id) {
            for (oi, ni) in old_indices.iter().zip(new_indices.iter()) {
                pairs.push((*oi, *ni));
            }
        }
    }
    pairs
}

/// Map `(original_index, key)` slices through LCS when the unmatched cross-product is small.
#[allow(dead_code)]
pub fn bounded_lcs_index_pairs<K: Eq + Clone>(
    old: &[(usize, K)],
    new: &[(usize, K)],
) -> Vec<(usize, usize)> {
    if old.is_empty() || new.is_empty() {
        return Vec::new();
    }
    if old.len() * new.len() > LCS_THRESHOLD {
        return Vec::new();
    }
    let old_keys: Vec<K> = old.iter().map(|(_, key)| key.clone()).collect();
    let new_keys: Vec<K> = new.iter().map(|(_, key)| key.clone()).collect();
    lcs_pairs(&old_keys, &new_keys)
        .into_iter()
        .map(|(oi, ni)| (old[oi].0, new[ni].0))
        .collect()
}

/// Pair unmatched indices whose fingerprint bucket has size one on each side.
pub fn fingerprint_bucket_pairs<F: Eq + Hash + Clone>(
    old: &[(usize, F)],
    new: &[(usize, F)],
) -> Vec<(usize, usize)> {
    let mut old_buckets: HashMap<F, Vec<usize>> = HashMap::new();
    for (oi, fp) in old {
        old_buckets.entry(fp.clone()).or_default().push(*oi);
    }
    let mut new_buckets: HashMap<F, Vec<usize>> = HashMap::new();
    for (ni, fp) in new {
        new_buckets.entry(fp.clone()).or_default().push(*ni);
    }
    let mut pairs = Vec::new();
    for (fp, old_indices) in &old_buckets {
        if old_indices.len() != 1 {
            continue;
        }
        if let Some(new_indices) = new_buckets.get(fp)
            && new_indices.len() == 1
        {
            pairs.push((old_indices[0], new_indices[0]));
        }
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_order_bucket_pairs_duplicate_keys() {
        let old = vec!['a', 'a', 'b'];
        let new = vec!['a', 'a', 'b'];
        let pairs = pair_in_order_by_key(&old, &new);
        let mut sorted = pairs.clone();
        sorted.sort();
        assert_eq!(sorted, vec![(0, 0), (1, 1), (2, 2)]);
    }

    #[test]
    fn unique_bijective_pairs_only_singleton_buckets() {
        let old = vec!['a', 'b', 'a'];
        let new = vec!['a', 'c', 'b'];
        let pairs = pair_unique_bijective(&old, &new);
        assert_eq!(pairs, vec![(1, 2)]);
    }

    #[test]
    fn bounded_lcs_skips_wide_remainder() {
        let old: Vec<(usize, char)> = (0..10).map(|i| (i, 'x')).collect();
        let new: Vec<(usize, char)> = (0..10).map(|i| (i + 10, 'x')).collect();
        assert!(bounded_lcs_index_pairs(&old, &new).is_empty());
    }

    #[test]
    fn bounded_lcs_pairs_small_remainder() {
        let old = vec![(0, 'a'), (1, 'b'), (2, 'c')];
        let new = vec![(0, 'a'), (1, 'c'), (2, 'b')];
        let pairs = bounded_lcs_index_pairs(&old, &new);
        assert_eq!(pairs.len(), 2);
    }

    #[test]
    fn equal_node_id_pairs_duplicate_siblings_in_order() {
        let id = 42u32;
        let old = vec![id, 1, id, 2, id];
        let new = vec![id, 1, id, 2, id];
        let mut pairs = pair_equal_node_ids(&old, &new);
        pairs.sort();
        assert_eq!(pairs, vec![(0, 0), (1, 1), (2, 2), (3, 3), (4, 4)]);
    }

    #[test]
    fn equal_node_id_pairs_uneven_duplicate_counts() {
        let id = 42u32;
        let old = vec![id, id, 1];
        let new = vec![id, 2, 3];
        let mut pairs = pair_equal_node_ids(&old, &new);
        pairs.sort();
        assert_eq!(pairs, vec![(0, 0)]);
    }

    #[test]
    fn equal_node_id_pairs_extra_new_duplicate() {
        let id = 42u32;
        let old = vec![id, 1];
        let new = vec![id, id, 2];
        let mut pairs = pair_equal_node_ids(&old, &new);
        pairs.sort();
        assert_eq!(pairs, vec![(0, 0)]);
    }
}
