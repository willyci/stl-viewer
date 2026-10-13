//! Minimal scoped-thread helpers for the data-parallel parts of mesh processing.

use std::ops::Range;

/// More threads than this do not help: the parallel passes are memory-bound.
const MAX_THREADS: usize = 8;

/// Worker threads to use for one data-parallel job.
pub(crate) fn threads() -> usize {
    std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(MAX_THREADS)
}

/// `0..len` cut into at most `parts` consecutive ranges of `chunk_len(len, parts)` items
/// (the last one may be shorter), so they line up with `slice.chunks_mut(chunk_len)`.
pub(crate) fn ranges(len: usize, parts: usize) -> Vec<Range<usize>> {
    let chunk = chunk_len(len, parts);
    (0..len)
        .step_by(chunk)
        .map(|s| s..(s + chunk).min(len))
        .collect()
}

pub(crate) fn chunk_len(len: usize, parts: usize) -> usize {
    len.div_ceil(parts.max(1)).max(1)
}

/// Runs `f` on every item on its own scoped thread; results come back in item order.
/// A panicking worker re-raises its panic on the calling thread.
pub(crate) fn map<T, R, F>(items: impl IntoIterator<Item = T>, f: F) -> Vec<R>
where
    T: Send,
    R: Send,
    F: Fn(T) -> R + Sync,
{
    std::thread::scope(|s| {
        let f = &f;
        let handles: Vec<_> = items
            .into_iter()
            .map(|item| s.spawn(move || f(item)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_cover_everything_once() {
        for (len, parts) in [(0, 4), (1, 4), (7, 3), (9, 3), (10, 1), (5, 0)] {
            let r = ranges(len, parts);
            assert!(r.len() <= parts.max(1));
            assert_eq!(r.iter().map(|r| r.len()).sum::<usize>(), len);
            assert!(r.windows(2).all(|w| w[0].end == w[1].start));
            let chunks = vec![0u8; len];
            let lens: Vec<usize> = chunks
                .chunks(chunk_len(len, parts))
                .map(<[u8]>::len)
                .collect();
            assert_eq!(lens, r.iter().map(|r| r.len()).collect::<Vec<_>>());
        }
    }

    #[test]
    fn map_keeps_order() {
        assert_eq!(map(0..5, |i| i * 2), vec![0, 2, 4, 6, 8]);
    }
}
