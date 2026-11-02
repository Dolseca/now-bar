//! Opt-in, low-overhead Windows performance sampling. No polling happens while disabled.
//! CPU and memory use kernel APIs; GPU usage uses the Windows GPU Engine PDH counter when present.
//! Temperature sensors are intentionally not guessed or synthesized.

use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::{null, null_mut};
use std::thread;
use std::time::Duration;

use crate::activities::live::{LiveMsg, LiveState};
use crate::platform::ipc::{self, Command};
use crate::settings;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FileTime {
    low: u32,
    high: u32,
}

impl FileTime {
    fn value(self) -> u64 {
        ((self.high as u64) << 32) | self.low as u64
    }
}

#[repr(C)]
struct MemoryStatusEx {
    length: u32,
    memory_load: u32,
    total_phys: u64,
    avail_phys: u64,
    total_page_file: u64,
    avail_page_file: u64,
    total_virtual: u64,
    avail_virtual: u64,
    avail_extended_virtual: u64,
}

#[link(name = "kernel32")]
extern "system" {
    fn GetSystemTimes(idle: *mut FileTime, kernel: *mut FileTime, user: *mut FileTime) -> i32;
    fn GlobalMemoryStatusEx(status: *mut MemoryStatusEx) -> i32;
}

#[derive(Default)]
struct CpuSampler {
    previous: Option<(u64, u64, u64)>,
}

impl CpuSampler {
    fn sample(&mut self) -> Option<u8> {
        let (mut idle, mut kernel, mut user) = (FileTime::default(), FileTime::default(), FileTime::default());
        if unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } == 0 {
            return None;
        }
        let current = (idle.value(), kernel.value(), user.value());
        let result = self.previous.and_then(|(old_idle, old_kernel, old_user)| {
            let idle_delta = current.0.saturating_sub(old_idle);
            let total_delta = current.1.saturating_sub(old_kernel).saturating_add(current.2.saturating_sub(old_user));
            (total_delta > 0).then(|| {
                let busy = total_delta.saturating_sub(idle_delta.min(total_delta));
                ((busy as f64 / total_delta as f64) * 100.0).round().clamp(0.0, 100.0) as u8
            })
        });
        self.previous = Some(current);
        result
    }
}

fn memory_percent() -> Option<u8> {
    let mut status: MemoryStatusEx = unsafe { std::mem::zeroed() };
    status.length = size_of::<MemoryStatusEx>() as u32;
    (unsafe { GlobalMemoryStatusEx(&mut status) } != 0).then_some(status.memory_load.min(100) as u8)
}

#[repr(C)]
union PdhValue {
    long_value: i32,
    double_value: f64,
    large_value: i64,
    ansi_string: *mut i8,
    wide_string: *mut u16,
}

#[repr(C)]
struct PdhFormattedValue {
    status: u32,
    value: PdhValue,
}

#[repr(C)]
struct PdhFormattedItemW {
    name: *mut u16,
    value: PdhFormattedValue,
}

#[link(name = "pdh")]
extern "system" {
    fn PdhOpenQueryW(source: *const u16, user_data: usize, query: *mut *mut c_void) -> u32;
    fn PdhAddEnglishCounterW(query: *mut c_void, path: *const u16, user_data: usize, counter: *mut *mut c_void) -> u32;
    fn PdhCollectQueryData(query: *mut c_void) -> u32;
    fn PdhGetFormattedCounterArrayW(counter: *mut c_void, format: u32, buffer_size: *mut u32, item_count: *mut u32, items: *mut PdhFormattedItemW) -> u32;
    fn PdhCloseQuery(query: *mut c_void) -> u32;
}

const PDH_FMT_DOUBLE: u32 = 0x0000_0200;
const PDH_MORE_DATA: u32 = 0x8000_07D2;
const PDH_VALID_DATA: u32 = 0;
const PDH_NEW_DATA: u32 = 1;

struct GpuSampler {
    query: *mut c_void,
    counter: *mut c_void,
}

impl GpuSampler {
    fn new() -> Option<Self> {
        use std::os::windows::ffi::OsStrExt;
        let mut query = null_mut();
        if unsafe { PdhOpenQueryW(null(), 0, &mut query) } != 0 || query.is_null() {
            return None;
        }
        let path: Vec<u16> = std::ffi::OsStr::new(r"\GPU Engine(*)\Utilization Percentage")
            .encode_wide().chain(std::iter::once(0)).collect();
        let mut counter = null_mut();
        if unsafe { PdhAddEnglishCounterW(query, path.as_ptr(), 0, &mut counter) } != 0 || counter.is_null() {
            unsafe { PdhCloseQuery(query) };
            return None;
        }
        let _ = unsafe { PdhCollectQueryData(query) }; // Prime the rate counter.
        Some(Self { query, counter })
    }

    fn sample(&mut self) -> Option<u8> {
        if unsafe { PdhCollectQueryData(self.query) } != 0 {
            return None;
        }
        let (mut bytes, mut count) = (0u32, 0u32);
        let first = unsafe { PdhGetFormattedCounterArrayW(self.counter, PDH_FMT_DOUBLE, &mut bytes, &mut count, null_mut()) };
        if first != PDH_MORE_DATA && first != 0 {
            return None;
        }
        if count == 0 || bytes == 0 {
            return Some(0);
        }
        // Vec<u64> provides the alignment required by PDH's C structs.
        let mut storage = vec![0u64; (bytes as usize).div_ceil(size_of::<u64>())];
        let status = unsafe {
            PdhGetFormattedCounterArrayW(
                self.counter, PDH_FMT_DOUBLE, &mut bytes, &mut count,
                storage.as_mut_ptr().cast::<PdhFormattedItemW>(),
            )
        };
        if status != 0 {
            return None;
        }
        let items = storage.as_ptr().cast::<PdhFormattedItemW>();
        let mut per_engine: HashMap<String, f64> = HashMap::new();
        for index in 0..count as usize {
            let item = unsafe { &*items.add(index) };
            if item.value.status != PDH_VALID_DATA && item.value.status != PDH_NEW_DATA {
                continue;
            }
            let value = unsafe { item.value.value.double_value };
            if !value.is_finite() || value <= 0.0 {
                continue;
            }
            let name = unsafe { wide_string(item.name) };
            let key = name.find("luid_").map(|at| name[at..].to_string()).unwrap_or(name);
            *per_engine.entry(key).or_default() += value;
        }
        let busiest = per_engine.values().copied().fold(0.0f64, f64::max);
        Some(busiest.round().clamp(0.0, 100.0) as u8)
    }
}

impl Drop for GpuSampler {
    fn drop(&mut self) {
        if !self.query.is_null() {
            unsafe { PdhCloseQuery(self.query) };
        }
    }
}

unsafe fn wide_string(pointer: *const u16) -> String {
    if pointer.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while len < 512 && unsafe { *pointer.add(len) } != 0 {
        len += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(pointer, len) })
}

fn clear_message() -> LiveMsg {
    LiveMsg {
        id: "nowbar:system-monitor".into(),
        title: "System".into(),
        text: String::new(),
        progress: None,
        state: LiveState::Clear,
        claude: None,
        app: None,
    }
}

pub fn start() {
    let _ = thread::Builder::new().name("nowbar-system-monitor".into()).spawn(|| {
        let mut cpu = CpuSampler::default();
        let mut gpu: Option<GpuSampler> = None;
        let mut gpu_retry_at: Option<std::time::Instant> = None;
        let mut visible = false;

        loop {
            let s = settings::current();
            let active = s.enabled && s.live_enabled && s.performance_enabled
                && (s.performance_cpu || s.performance_memory || s.performance_gpu);
            if !active {
                if visible {
                    let _ = ipc::send(&Command::Live(Box::new(clear_message())));
                    visible = false;
                }
                cpu = CpuSampler::default();
                gpu = None;
                gpu_retry_at = None;
                thread::sleep(Duration::from_secs(2));
                continue;
            }

            if s.performance_gpu && gpu.is_none() && gpu_retry_at.is_none_or(|at| at.elapsed() >= Duration::from_secs(30)) {
                gpu = GpuSampler::new();
                gpu_retry_at = Some(std::time::Instant::now());
            } else if !s.performance_gpu {
                gpu = None;
                gpu_retry_at = None;
            }

            let mut details = Vec::with_capacity(3);
            if s.performance_cpu {
                let value = cpu.sample().map_or_else(|| "—".to_string(), |v| format!("{v}%"));
                details.push(format!("CPU: {value}"));
            }
            if s.performance_memory {
                let value = memory_percent().map_or_else(|| "—".to_string(), |v| format!("{v}%"));
                details.push(format!("RAM: {value}"));
            }
            if s.performance_gpu {
                let value = gpu.as_mut().and_then(GpuSampler::sample).map_or_else(|| "—".to_string(), |v| format!("{v}%"));
                details.push(format!("GPU: {value}"));
            }

            let msg = LiveMsg {
                id: "nowbar:system-monitor".into(),
                title: "System".into(),
                text: details.join(" · "),
                progress: None,
                state: LiveState::Run,
                claude: None,
                app: None,
            };
            // The generic live pill uses the text as its trailing status line for this activity.
            if ipc::send(&Command::Live(Box::new(msg))) {
                visible = true;
            }
            thread::sleep(Duration::from_secs(s.performance_interval_s.clamp(1, 10) as u64));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_status_layout_matches_windows_abi() {
        assert_eq!(size_of::<MemoryStatusEx>(), 64);
    }

    #[test]
    fn gpu_engine_name_groups_processes_by_adapter_and_engine() {
        let a = "pid_123_luid_0x00000000_0x0000B3E5_phys_0_eng_0_engtype_3D";
        let b = "pid_456_luid_0x00000000_0x0000B3E5_phys_0_eng_0_engtype_3D";
        let c = "pid_456_luid_0x00000000_0x0000B3E5_phys_0_eng_1_engtype_Copy";
        let key = |s: &str| s.find("luid_").map(|at| &s[at..]).unwrap_or(s);
        assert_eq!(key(a), key(b));
        assert_ne!(key(a), key(c));
    }
}
