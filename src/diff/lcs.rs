/// Longest common subsequence index pairs between two slices.
pub fn lcs_pairs<T: Eq>(a: &[T], b: &[T]) -> Vec<(usize, usize)> {
    let n = a.len();
    let m = b.len();
    if n == 0 || m == 0 {
        return Vec::new();
    }

    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for (i, ai) in a.iter().enumerate() {
        for (j, bj) in b.iter().enumerate() {
            if ai == bj {
                dp[i + 1][j + 1] = dp[i][j] + 1;
            } else {
                dp[i + 1][j + 1] = dp[i][j + 1].max(dp[i + 1][j]);
            }
        }
    }

    let mut pairs = Vec::new();
    let mut i = n;
    let mut j = m;
    while i > 0 && j > 0 {
        if a[i - 1] == b[j - 1] {
            pairs.push((i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] >= dp[i][j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    pairs.reverse();
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_common_subsequence() {
        let a = vec!['a', 'b', 'c', 'd'];
        let b = vec!['a', 'c', 'd', 'e'];
        let pairs = lcs_pairs(&a, &b);
        assert_eq!(pairs, vec![(0, 0), (2, 1), (3, 2)]);
    }

    #[test]
    fn empty_when_no_common() {
        assert!(lcs_pairs(&[1, 2], &[3, 4]).is_empty());
    }
}
