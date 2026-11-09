//! Summaries of timing samples.

use serde::{Deserialize, Serialize};

/// Min, median, mean and max of a set of samples, in nanoseconds.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub min_ns: u64,
    pub median_ns: u64,
    pub mean_ns: u64,
    pub max_ns: u64,
}

impl Summary {
    pub fn from_samples(samples: &[u64]) -> Option<Summary> {
        if samples.is_empty() {
            return None;
        }
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let n = sorted.len();
        let median_ns = if n % 2 == 1 {
            sorted[n / 2]
        } else {
            (sorted[n / 2 - 1] + sorted[n / 2]) / 2
        };
        let sum: u128 = sorted.iter().map(|&s| s as u128).sum();
        Some(Summary {
            min_ns: sorted[0],
            median_ns,
            mean_ns: (sum / n as u128) as u64,
            max_ns: sorted[n - 1],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarises_an_odd_number_of_samples() {
        let s = Summary::from_samples(&[30, 10, 20]).unwrap();
        assert_eq!(s, Summary { min_ns: 10, median_ns: 20, mean_ns: 20, max_ns: 30 });
    }

    #[test]
    fn median_of_an_even_number_of_samples_is_the_mean_of_the_middle_two() {
        let s = Summary::from_samples(&[4, 1, 10, 2]).unwrap();
        assert_eq!(s, Summary { min_ns: 1, median_ns: 3, mean_ns: 4, max_ns: 10 });
    }

    #[test]
    fn no_samples_has_no_summary() {
        assert_eq!(Summary::from_samples(&[]), None);
    }
}
