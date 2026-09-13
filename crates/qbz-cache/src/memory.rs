//! Memory headroom sampled by the cache worker, never by an audio callback.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MemoryHeadroom {
    pub total: u64,
    pub available: u64,
}

#[cfg(target_os = "linux")]
pub(crate) fn read() -> Option<MemoryHeadroom> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kib = |key: &str| -> Option<u64> {
        text.lines()
            .find_map(|line| line.strip_prefix(key))?
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()?
            .checked_mul(1024)
    };
    let mut memory = MemoryHeadroom {
        total: kib("MemTotal:")?,
        available: kib("MemAvailable:")?,
    };
    // Respect all enclosing cgroup-v2 limits, including a container's parent.
    if let Ok(groups) = std::fs::read_to_string("/proc/self/cgroup") {
        if let Some(group) = groups.lines().find_map(|line| line.strip_prefix("0::")) {
            let root = std::path::Path::new("/sys/fs/cgroup");
            let mut path = root.join(group.trim_start_matches('/'));
            while path.starts_with(root) {
                let read_number = |name| {
                    std::fs::read_to_string(path.join(name))
                        .ok()?
                        .trim()
                        .parse::<u64>()
                        .ok()
                };
                if let (Some(limit), Some(used)) =
                    (read_number("memory.max"), read_number("memory.current"))
                {
                    memory.total = memory.total.min(limit);
                    memory.available = memory.available.min(limit.saturating_sub(used));
                }
                if !path.pop() {
                    break;
                }
            }
        }
    }
    Some(memory)
}

#[cfg(target_os = "macos")]
pub(crate) fn read() -> Option<MemoryHeadroom> {
    let total = std::process::Command::new("/usr/sbin/sysctl")
        .args(["-n", "hw.memsize", "kern.memorystatus_vm_pressure_level"])
        .output()
        .ok()?;
    let pages = std::process::Command::new("/usr/bin/vm_stat")
        .output()
        .ok()?;
    if !total.status.success() || !pages.status.success() {
        return None;
    }
    let info = std::str::from_utf8(&total.stdout).ok()?;
    let mut info = info.lines();
    parse_macos(
        info.next()?.trim().parse().ok()?,
        std::str::from_utf8(&pages.stdout).ok()?,
        info.next()?.trim().parse().ok()?,
    )
}

#[cfg(any(target_os = "macos", test))]
fn parse_macos(total: u64, text: &str, pressure: u32) -> Option<MemoryHeadroom> {
    let page_size = text
        .lines()
        .next()?
        .split("page size of ")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    let pages = |key| {
        text.lines()
            .find_map(|line| line.strip_prefix(key))?
            .trim()
            .trim_end_matches('.')
            .parse::<u64>()
            .ok()
    };
    // sysctl exports dispatch flags, not XNU's internal 0..4 enum:
    // 1 = normal, 2 = warning, 4 = critical. Unknown/missing metrics grant nothing.
    // File-backed pages are reclaimable; do not count anonymous inactive pages
    // or compressed memory. Counting only free pages misclassifies healthy Macs.
    let available = match pressure {
        1 => pages("Pages free:")?
            .checked_add(pages("File-backed pages:")?)?
            .checked_mul(page_size)?
            .min(total),
        2 | 4 => 0,
        _ => return None,
    };
    Some(MemoryHeadroom { total, available })
}

#[cfg(target_os = "windows")]
pub(crate) fn read() -> Option<MemoryHeadroom> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    unsafe { GlobalMemoryStatusEx(&mut status) }.ok()?;
    Some(MemoryHeadroom {
        total: status.ullTotalPhys,
        available: status.ullAvailPhys,
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub(crate) fn read() -> Option<MemoryHeadroom> {
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn macos_counts_reclaimable_files_but_honors_real_pressure() {
        let text = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\nPages free: 4058.\nPages inactive: 161060.\nFile-backed pages: 119431.\nPages occupied by compressor: 97493.\n";
        let m = super::parse_macos(8 << 30, text, 1).unwrap();
        assert_eq!(m.available, (4058 + 119431) * 16384);
        assert_eq!(super::parse_macos(8 << 30, text, 2).unwrap().available, 0);
        assert_eq!(super::parse_macos(8 << 30, text, 4).unwrap().available, 0);
        assert!(super::parse_macos(8 << 30, text, 0).is_none());
        assert!(super::parse_macos(8 << 30, "unavailable", 1).is_none());
    }
}
