//! Sets of characters as sorted inclusive ranges, and the lookup structure
//! the matchers use (TASK-100.31).
//!
//! A character is a code point in unicode mode and a code unit otherwise;
//! the set does not care, the parser decides the universe it complements
//! against.

/// The largest code point.
pub(crate) const MAX_CODE_POINT: u32 = 0x10FFFF;

/// A set of characters: sorted, non-overlapping, non-adjacent inclusive
/// ranges.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct CharSet {
    ranges: Vec<(u32, u32)>,
}

impl CharSet {
    pub(crate) fn new() -> CharSet {
        CharSet { ranges: Vec::new() }
    }

    /// From ranges that are already sorted and merged (the generated
    /// tables).
    pub(crate) fn from_sorted(ranges: &[(u32, u32)]) -> CharSet {
        CharSet {
            ranges: ranges.to_vec(),
        }
    }

    /// From ranges in any order, overlapping or not.
    pub(crate) fn from_unsorted(mut ranges: Vec<(u32, u32)>) -> CharSet {
        ranges.sort_unstable();
        let mut out: Vec<(u32, u32)> = Vec::with_capacity(ranges.len());
        for (lo, hi) in ranges {
            match out.last_mut() {
                Some(last) if lo <= last.1.saturating_add(1) => last.1 = last.1.max(hi),
                _ => out.push((lo, hi)),
            }
        }
        CharSet { ranges: out }
    }

    pub(crate) fn single(c: u32) -> CharSet {
        CharSet {
            ranges: vec![(c, c)],
        }
    }

    pub(crate) fn range(lo: u32, hi: u32) -> CharSet {
        CharSet {
            ranges: vec![(lo, hi)],
        }
    }

    pub(crate) fn ranges(&self) -> &[(u32, u32)] {
        &self.ranges
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// The only member, when there is exactly one.
    pub(crate) fn single_member(&self) -> Option<u32> {
        match self.ranges.as_slice() {
            [(a, b)] if a == b => Some(*a),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> u64 {
        self.ranges.iter().map(|&(a, b)| (b - a) as u64 + 1).sum()
    }

    pub(crate) fn contains(&self, c: u32) -> bool {
        let r = &self.ranges;
        let i = r.partition_point(|&(_, hi)| hi < c);
        i < r.len() && r[i].0 <= c
    }

    pub(crate) fn union(&self, other: &CharSet) -> CharSet {
        let mut v = Vec::with_capacity(self.ranges.len() + other.ranges.len());
        v.extend_from_slice(&self.ranges);
        v.extend_from_slice(&other.ranges);
        CharSet::from_unsorted(v)
    }

    /// Every character up to `max` that is not in the set.
    pub(crate) fn complement(&self, max: u32) -> CharSet {
        let mut out = Vec::with_capacity(self.ranges.len() + 1);
        let mut next = 0u32;
        for &(lo, hi) in &self.ranges {
            if lo > max {
                break;
            }
            if lo > next {
                out.push((next, lo - 1));
            }
            next = hi.saturating_add(1);
        }
        if next <= max {
            out.push((next, max));
        }
        CharSet { ranges: out }
    }

    pub(crate) fn intersect(&self, other: &CharSet) -> CharSet {
        let (a, b) = (&self.ranges, &other.ranges);
        let (mut i, mut j) = (0, 0);
        let mut out = Vec::new();
        while i < a.len() && j < b.len() {
            let lo = a[i].0.max(b[j].0);
            let hi = a[i].1.min(b[j].1);
            if lo <= hi {
                out.push((lo, hi));
            }
            if a[i].1 < b[j].1 {
                i += 1;
            } else {
                j += 1;
            }
        }
        CharSet { ranges: out }
    }

    pub(crate) fn subtract(&self, other: &CharSet) -> CharSet {
        self.intersect(&other.complement(MAX_CODE_POINT))
    }

    /// Whether the two sets share a member.
    pub(crate) fn overlaps(&self, other: &CharSet) -> bool {
        let (a, b) = (&self.ranges, &other.ranges);
        let (mut i, mut j) = (0, 0);
        while i < a.len() && j < b.len() {
            if a[i].0.max(b[j].0) <= a[i].1.min(b[j].1) {
                return true;
            }
            if a[i].1 < b[j].1 {
                i += 1;
            } else {
                j += 1;
            }
        }
        false
    }
}

/// A [`CharSet`] laid out for matching: a bitmap answers Latin-1 in one
/// load, a binary search over the remaining ranges answers the rest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SetMatcher {
    latin1: [u64; 4],
    /// Ranges that reach past U+00FF (clipped to start at U+0100).
    high: Box<[(u32, u32)]>,
}

impl SetMatcher {
    /// The bytes this matcher holds, itself included.
    pub(crate) fn heap_bytes(&self) -> usize {
        std::mem::size_of::<SetMatcher>() + std::mem::size_of_val(&*self.high)
    }

    pub(crate) fn new(set: &CharSet) -> SetMatcher {
        let mut latin1 = [0u64; 4];
        let mut high = Vec::new();
        for &(lo, hi) in set.ranges() {
            let mut c = lo;
            while c <= hi && c < 256 {
                latin1[(c >> 6) as usize] |= 1 << (c & 63);
                c += 1;
            }
            if hi >= 256 {
                high.push((lo.max(256), hi));
            }
        }
        SetMatcher {
            latin1,
            high: high.into_boxed_slice(),
        }
    }

    #[inline]
    pub(crate) fn contains(&self, c: u32) -> bool {
        if c < 256 {
            return self.latin1[(c >> 6) as usize] & (1 << (c & 63)) != 0;
        }
        let r = &self.high;
        match r.len() {
            0 => false,
            1 => r[0].0 <= c && c <= r[0].1,
            _ => {
                let i = r.partition_point(|&(_, hi)| hi < c);
                i < r.len() && r[i].0 <= c
            }
        }
    }

    /// The set back as ranges (for serialization).
    pub(crate) fn to_set(&self) -> CharSet {
        let mut v = Vec::new();
        for c in 0..256u32 {
            if self.contains(c) {
                v.push((c, c));
            }
        }
        v.extend_from_slice(&self.high);
        CharSet::from_unsorted(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_operations() {
        let a = CharSet::from_unsorted(vec![(5, 10), (1, 3), (4, 4), (20, 30)]);
        assert_eq!(a.ranges(), &[(1, 10), (20, 30)]);
        let b = CharSet::from_unsorted(vec![(8, 22)]);
        assert_eq!(a.intersect(&b).ranges(), &[(8, 10), (20, 22)]);
        assert_eq!(a.subtract(&b).ranges(), &[(1, 7), (23, 30)]);
        assert_eq!(a.union(&b).ranges(), &[(1, 30)]);
        assert_eq!(a.complement(40).ranges(), &[(0, 0), (11, 19), (31, 40)]);
        assert!(a.contains(1) && a.contains(25) && !a.contains(15) && !a.contains(0));
        assert!(a.overlaps(&b) && !a.overlaps(&CharSet::range(11, 19)));
        let m = SetMatcher::new(&CharSet::from_unsorted(vec![
            (0x41, 0x5A),
            (0xF0, 0x110),
            (0x1F600, 0x1F64F),
        ]));
        for c in [0x41, 0x5A, 0xF0, 0xFF, 0x100, 0x110, 0x1F600] {
            assert!(m.contains(c), "{c:x}");
        }
        for c in [0x40, 0x5B, 0xEF, 0x111, 0x1F650] {
            assert!(!m.contains(c), "{c:x}");
        }
        assert_eq!(
            m.to_set().ranges(),
            &[(0x41, 0x5A), (0xF0, 0x110), (0x1F600, 0x1F64F)]
        );
    }
}
