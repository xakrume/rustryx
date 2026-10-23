//! CPU counts used to resolve `0 = auto` limits.

/// Logical CPUs available to the process.
///
/// `available_parallelism` honours cgroup CPU quotas, which is what limits the
/// work a container can actually do.
#[must_use]
pub fn logical_cpus() -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
}

/// Physical cores, never more than the CPUs the process may use.
///
/// `num_cpus::get_physical` reports the host, so inside a pod limited to 2 CPUs on
/// a 64 core node it would size the password semaphore at 32 and let argon2id
/// oversubscribe the quota; capping by [`logical_cpus`] prevents that.
#[must_use]
pub fn physical_cores() -> usize {
    num_cpus::get_physical().clamp(1, logical_cpus().max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_cores_is_positive_and_bounded_by_logical() {
        assert!(physical_cores() >= 1);
        assert!(physical_cores() <= logical_cpus());
    }
}
