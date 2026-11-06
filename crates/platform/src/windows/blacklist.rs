use std::collections::HashMap;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::{LazyLock, RwLock};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use windows::Win32::Foundation::{HANDLE, MAX_PATH};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
use windows::core::PWSTR;

struct Process {
    name: String,
    handle: OwnedHandle,
}

impl Process {
    fn query(pid: u32) -> Option<Self> {
        let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
        let handle = unsafe { OwnedHandle::from_raw_handle(raw.0) };
        let mut buf = [0u16; MAX_PATH as usize];
        let mut len = buf.len() as u32;
        unsafe { QueryFullProcessImageNameW(raw, PROCESS_NAME_FORMAT(0), PWSTR(buf.as_mut_ptr()), &mut len) }.ok()?;
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        let name = path.rsplit(['\\', '/']).next()?.to_ascii_lowercase();
        Some(Self { name, handle })
    }

    fn running(&self) -> bool {
        let mut code = 0;
        unsafe { GetExitCodeProcess(HANDLE(self.handle.as_raw_handle()), &mut code) }.is_ok() && code == 259
    }
}

#[derive(Default)]
struct Cache {
    names: Vec<String>,
    processes: HashMap<u32, Process>,
}

static CACHE: LazyLock<RwLock<Cache>> = LazyLock::new(|| RwLock::new(Cache::default()));
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// 只更新名单，不枚举系统进程；进程名与名单独立缓存。
pub fn replace(names: Vec<String>) {
    let names: Vec<_> = names.into_iter().map(|s| s.trim().to_ascii_lowercase()).collect();
    let mut cache = CACHE.write().unwrap();
    cache.names = names;
    ACTIVE.store(!cache.names.is_empty(), Relaxed);
}

pub fn active() -> bool {
    ACTIVE.load(Relaxed)
}

/// 与识别引擎一致使用前台程序。钩子不做跨窗口命中测试或定时进程枚举。
pub fn foreground() -> bool {
    if !active() {
        return false;
    }
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut pid)) };
    if pid == 0 {
        return false;
    }
    {
        let cache = CACHE.read().unwrap();
        if let Some(process) = cache.processes.get(&pid)
            && process.running()
        {
            return cache.names.iter().any(|name| name == &process.name);
        }
    }
    // 每个新进程只查询一次，保留句柄核对生命周期，避免 PID 重用导致误匹配。
    let Some(process) = Process::query(pid) else { return false };
    let mut cache = CACHE.write().unwrap();
    let blocked = cache.names.iter().any(|name| name == &process.name);
    // 限制保留句柄数量；释放在锁外进行，避免 CloseHandle 阻塞其他线程。
    let old = if cache.processes.len() >= 128 { std::mem::take(&mut cache.processes) } else { HashMap::new() };
    let previous = cache.processes.insert(pid, process);
    drop(cache);
    drop(previous);
    drop(old);
    blocked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_cache_queries_name_and_retains_a_live_handle() {
        let process = Process::query(std::process::id()).unwrap();
        let exe = std::env::current_exe().unwrap().file_name().unwrap().to_string_lossy().to_ascii_lowercase();
        assert_eq!(process.name, exe);
        assert!(process.running());
    }

    #[test]
    #[ignore = "manual desktop timing probe; run alone with --ignored --nocapture"]
    fn compare_blacklist_lookup_latency() {
        use std::time::Instant;
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

        let mut pt = POINT::default();
        unsafe { GetCursorPos(&mut pt) }.unwrap();
        let pt = (pt.x, pt.y);
        replace(vec!["__nextgestures_latency_probe__.exe".into()]);
        foreground();
        let old = || { std::hint::black_box(super::super::window::exe_at_point(pt)); };
        let cached = || { std::hint::black_box(foreground()); };
        fn sample(label: &str, op: impl Fn()) {
            let mut samples = Vec::new();
            for _ in 0..300 {
                let start = Instant::now();
                op();
                samples.push(start.elapsed().as_nanos() as f64 / 1000.0);
            }
            samples.sort_by(f64::total_cmp);
            println!("{label}: p50={:.2}us p95={:.2}us max={:.2}us", samples[150], samples[285], samples[299]);
        }
        sample("old synchronous process query", old);
        sample("cached process lookup", cached);
        replace(vec![]);
        sample("empty blacklist", cached);
    }
}
