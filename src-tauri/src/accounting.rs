use vstd::prelude::*;

verus! {

spec fn cap_bytes(bytes: int) -> int {
    if bytes > u64::MAX { u64::MAX as int } else { bytes }
}

// Keep the largest copy out of the running sum so the full group size can
// exceed u64::MAX while the reclaimable part still fits.
struct ReclaimableBytes {
    largest: u64,
    others: u64,
    #[allow(dead_code)] // Read by Verus only.
    total: Ghost<int>,
}

impl ReclaimableBytes {
    spec fn valid(self) -> bool {
        self.total@ >= self.largest && self.others == cap_bytes(self.total@ - self.largest)
    }

    fn new() -> (result: Self)
        ensures result.valid(), result.total@ == 0, result.largest == 0,
    {
        Self { largest: 0, others: 0, total: Ghost(0int) }
    }

    fn add(self, size: u64) -> (result: Self)
        requires self.valid(),
        ensures
            result.valid(),
            result.total@ == self.total@ + size,
            result.largest == if size > self.largest { size } else { self.largest },
    {
        let (largest, smaller) = if size > self.largest {
            (size, self.largest)
        } else {
            (self.largest, size)
        };
        Self {
            largest,
            others: self.others.saturating_add(smaller),
            total: Ghost(self.total@ + size),
        }
    }
}

pub(crate) fn exact_group_bytes(size: u64, copies: u64) -> (bytes: u64)
    ensures bytes == cap_bytes(size * (if copies > 0 { copies - 1 } else { 0 })),
{
    size.saturating_mul(copies.saturating_sub(1))
}

} // verus!

pub(crate) fn reclaimable_bytes(sizes: impl Iterator<Item = u64>) -> u64 {
    sizes
        .fold(ReclaimableBytes::new(), ReclaimableBytes::add)
        .others
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use proptest::prelude::*;

    /// File sizes that mix small values with values near `u64::MAX`, so
    /// sums overflow often enough to exercise the saturating paths.
    pub(crate) fn file_size() -> impl Strategy<Value = u64> {
        prop_oneof![0u64..1_000, (u64::MAX - 1_000)..=u64::MAX, any::<u64>()]
    }

    /// Reference result in wide arithmetic, independent of the accumulator.
    pub(crate) fn expected_reclaimable_bytes(sizes: &[u64]) -> u64 {
        let total: u128 = sizes.iter().copied().map(u128::from).sum();
        let max = sizes.iter().copied().max().unwrap_or(0);
        (total - u128::from(max)).min(u128::from(u64::MAX)) as u64
    }

    proptest! {
        #[test]
        fn reclaimable_bytes_matches_wide_arithmetic_in_any_order(
            sizes in prop::collection::vec(file_size(), 0..8),
        ) {
            let expected = expected_reclaimable_bytes(&sizes);
            prop_assert_eq!(reclaimable_bytes(sizes.iter().copied()), expected);
            prop_assert_eq!(reclaimable_bytes(sizes.iter().rev().copied()), expected);
        }

        #[test]
        fn exact_group_bytes_matches_reclaimable_bytes_of_equal_copies(
            size in file_size(),
            copies in 0u64..8,
        ) {
            let sizes = vec![size; copies as usize];
            prop_assert_eq!(exact_group_bytes(size, copies), expected_reclaimable_bytes(&sizes));
            prop_assert_eq!(exact_group_bytes(size, copies), reclaimable_bytes(sizes.into_iter()));
        }

        #[test]
        fn exact_group_bytes_saturates_for_huge_copy_counts(size in file_size(), copies in any::<u64>()) {
            let expected = (u128::from(size) * u128::from(copies.saturating_sub(1)))
                .min(u128::from(u64::MAX)) as u64;
            prop_assert_eq!(exact_group_bytes(size, copies), expected);
        }
    }
}
