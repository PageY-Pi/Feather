//! Process memory, measured rather than estimated.
//!
//! Peak working set is the number a user actually feels: it is what Task Manager
//! shows and what triggers "this thing is eating my RAM". We deliberately do not
//! report allocation counts here — an allocation counter needs a global allocator
//! hook that would perturb the very timings measured next to it, and the ruler's
//! job is to be boring and trustworthy, not exhaustive.

/// Peak resident set size of this process, in KiB.
///
/// Returns 0 on a platform where the query is unavailable; callers print it
/// verbatim, so a zero is visibly "not measured" rather than silently wrong.
pub fn peak_rss_kb() -> u64 {
    platform_peak_rss_kb().unwrap_or(0)
}

#[cfg(windows)]
fn platform_peak_rss_kb() -> Option<u64> {
    // PROCESS_MEMORY_COUNTERS_EX, declared locally so the bench needs no winapi
    // dependency. The layout is stable ABI and matches the SDK header.
    #[repr(C)]
    struct ProcessMemoryCountersEx {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
        private_usage: usize,
    }
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(
            process: isize,
            counters: *mut ProcessMemoryCountersEx,
            cb: u32,
        ) -> i32;
    }
    let mut counters = ProcessMemoryCountersEx {
        cb: std::mem::size_of::<ProcessMemoryCountersEx>() as u32,
        page_fault_count: 0,
        peak_working_set_size: 0,
        working_set_size: 0,
        quota_peak_paged_pool_usage: 0,
        quota_paged_pool_usage: 0,
        quota_peak_non_paged_pool_usage: 0,
        quota_non_paged_pool_usage: 0,
        pagefile_usage: 0,
        peak_pagefile_usage: 0,
        private_usage: 0,
    };
    // SAFETY: GetCurrentProcess returns a pseudo-handle that must not be closed;
    // the struct is fully initialized and `cb` reports its real size.
    let ok = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters,
            std::mem::size_of::<ProcessMemoryCountersEx>() as u32,
        )
    };
    (ok != 0).then(|| (counters.peak_working_set_size / 1024) as u64)
}

#[cfg(target_os = "linux")]
fn platform_peak_rss_kb() -> Option<u64> {
    // VmHWM is the kernel's own high-water mark, so reading it costs one open.
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(not(any(windows, target_os = "linux")))]
fn platform_peak_rss_kb() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The query must work on the platform this runs on, otherwise every number
    /// in the ruler's memory column is a silent zero.
    #[test]
    fn peak_rss_is_measurable() {
        if cfg!(any(windows, target_os = "linux")) {
            let kb = peak_rss_kb();
            assert!(kb > 0, "peak RSS should be readable, got {kb}");
        }
    }

    /// Peak is a high-water mark, so it must never go down as work is done.
    #[test]
    fn peak_rss_is_monotonic() {
        let first = peak_rss_kb();
        // Touch enough memory to guarantee a new high-water mark.
        let ballast: Vec<u8> = vec![1; 16 * 1024 * 1024];
        assert_eq!(ballast.len(), 16 * 1024 * 1024);
        let second = peak_rss_kb();
        assert!(
            second >= first,
            "peak RSS must be monotonic: {first} -> {second}"
        );
    }
}
