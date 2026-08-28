//! Host machine introspection — logical CPU count, physical RAM, primary
//! monitor resolution. Mirrors `host_logical_cores` / `host_ram_gb` /
//! `host_ram_bucket_gb` in the launcher. All probes are best-effort.

use std::process::Command;

/// Primary monitor size in CSS pixels.
pub type Size = (u32, u32);

/// Logical CPU count (SMT threads). Falls back to 8.
pub fn host_logical_cores() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8)
}

fn cmd_out(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Physical RAM in GiB, best-effort per OS. `None` on failure.
#[allow(clippy::needless_return)]
pub fn host_ram_gb() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        let out = cmd_out("sysctl", &["-n", "hw.memsize"])?;
        let bytes: u64 = out.trim().parse().ok()?;
        if bytes == 0 {
            return None;
        }
        return Some(bytes / (1024 * 1024 * 1024));
    }
    #[cfg(target_os = "linux")]
    {
        let txt = std::fs::read_to_string("/proc/meminfo").ok()?;
        for line in txt.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
                if kb == 0 {
                    return None;
                }
                return Some(kb / (1024 * 1024));
            }
        }
        return None;
    }
    #[cfg(target_os = "windows")]
    {
        let out = cmd_out(
            "powershell",
            &[
                "-NoProfile",
                "-Command",
                "(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory",
            ],
        )?;
        let bytes: u64 = out.trim().parse().ok()?;
        if bytes == 0 {
            return None;
        }
        return Some(bytes / (1024 * 1024 * 1024));
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

/// Round host RAM to Chrome's deviceMemory bucket {8,16,32}; `None` → 16.
pub fn host_ram_bucket_gb() -> u64 {
    match host_ram_gb() {
        None => 16,
        Some(gb) if gb >= 32 => 32,
        Some(gb) if gb >= 16 => 16,
        Some(_) => 8,
    }
}

/// Primary monitor `(width, height)`, or `None` on failure.
#[allow(clippy::needless_return)]
pub fn host_screen_size() -> Option<Size> {
    #[cfg(target_os = "macos")]
    {
        let out = cmd_out("system_profiler", &["SPDisplaysDataType"])?;
        let mut best: Option<Size> = None;
        for line in out.lines() {
            if let Some(idx) = line.find("Resolution:") {
                let rest = &line[idx + "Resolution:".len()..];
                let nums: Vec<u32> = rest
                    .split(|c: char| !c.is_ascii_digit())
                    .filter(|s| !s.is_empty())
                    .filter_map(|s| s.parse().ok())
                    .collect();
                if nums.len() >= 2 {
                    let (w, h) = (nums[0], nums[1]);
                    if best.map(|(bw, bh)| w * h > bw * bh).unwrap_or(true) {
                        best = Some((w, h));
                    }
                }
            }
        }
        return best;
    }
    #[cfg(target_os = "windows")]
    {
        let out = cmd_out(
            "powershell",
            &[
                "-NoProfile",
                "-Command",
                "Add-Type -AssemblyName System.Windows.Forms; $s=[System.Windows.Forms.Screen]::PrimaryScreen.Bounds; \"$($s.Width)x$($s.Height)\"",
            ],
        )?;
        return parse_wxh(out.trim());
    }
    #[cfg(target_os = "linux")]
    {
        let out = cmd_out("xrandr", &["--query"])?;
        for line in out.lines() {
            if line.contains('*') {
                let token = line.split_whitespace().find(|t| t.contains('x'))?;
                return parse_wxh(token);
            }
        }
        return None;
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

#[allow(dead_code)]
fn parse_wxh(s: &str) -> Option<Size> {
    let s = s.trim();
    let (w, rest) = s.split_once('x')?;
    let h: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    Some((w.trim().parse().ok()?, h.parse().ok()?))
}
