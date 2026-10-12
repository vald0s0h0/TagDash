// Half-open millisecond intervals [start, end): the unit of tick coverage.
//
// Coverage is kept per (symbol, kind) as merged intervals, and a download only ever
// fetches `subtract(wanted, covered)` — which is what makes a settings change or a
// second strategy cost only the delta.

pub type Span = (i64, i64);

/// Sort and merge overlapping or touching spans; empty spans are dropped.
pub fn merge(mut spans: Vec<Span>) -> Vec<Span> {
    spans.retain(|(s, e)| e > s);
    spans.sort_unstable();
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for (s, e) in spans {
        match out.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// The parts of `want` not covered by `have`. Both may be unsorted/overlapping.
pub fn subtract(want: Vec<Span>, have: Vec<Span>) -> Vec<Span> {
    let have = merge(have);
    let mut out = Vec::new();
    for (mut s, e) in merge(want) {
        for &(hs, he) in &have {
            if he <= s {
                continue;
            }
            if hs >= e {
                break;
            }
            if hs > s {
                out.push((s, hs));
            }
            s = s.max(he);
            if s >= e {
                break;
            }
        }
        if s < e {
            out.push((s, e));
        }
    }
    out
}

/// Total length of merged spans.
pub fn total(spans: &[Span]) -> i64 {
    merge(spans.to_vec()).iter().map(|(s, e)| e - s).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_joins_overlaps_and_touches() {
        assert_eq!(
            merge(vec![(10, 20), (0, 5), (5, 8), (15, 30), (40, 40)]),
            vec![(0, 8), (10, 30)]
        );
    }

    #[test]
    fn subtract_keeps_only_the_delta() {
        assert_eq!(subtract(vec![(0, 100)], vec![]), vec![(0, 100)]);
        assert_eq!(subtract(vec![(0, 100)], vec![(0, 100)]), vec![]);
        assert_eq!(
            subtract(vec![(0, 100)], vec![(20, 30), (50, 60)]),
            vec![(0, 20), (30, 50), (60, 100)]
        );
        assert_eq!(subtract(vec![(0, 100)], vec![(-10, 40)]), vec![(40, 100)]);
        assert_eq!(subtract(vec![(0, 100)], vec![(90, 200)]), vec![(0, 90)]);
        assert_eq!(
            subtract(vec![(0, 10), (20, 30)], vec![(5, 25)]),
            vec![(0, 5), (25, 30)]
        );
    }

    #[test]
    fn widening_a_window_fetches_only_the_new_edges() {
        let before = vec![(1_000, 2_000)];
        let after = vec![(500, 2_600)];
        assert_eq!(subtract(after, before), vec![(500, 1_000), (2_000, 2_600)]);
    }

    #[test]
    fn total_counts_merged_length() {
        assert_eq!(total(&[(0, 10), (5, 15), (20, 25)]), 20);
    }
}
