pub(super) fn worker_count(requested: usize, available_parallelism: usize) -> usize {
    requested.max(1).min(available_parallelism.max(1))
}

#[cfg(test)]
mod tests {
    #[test]
    fn worker_count_never_exceeds_cpu_budget_or_reaches_zero() {
        assert_eq!(super::worker_count(0, 8), 1);
        assert_eq!(super::worker_count(2, 8), 2);
        assert_eq!(super::worker_count(99, 4), 4);
        assert_eq!(super::worker_count(99, 0), 1);
    }
}
