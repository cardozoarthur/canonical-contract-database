use crate::common::Result;
#[cfg(windows)]
pub fn process_metrics() -> Result<(f64, u64, u64)> {
    use windows_sys::Win32::{
        Foundation::FILETIME,
        System::{
            ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
            Threading::{GetCurrentProcess, GetProcessTimes},
        },
    };
    let mut times = [FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    }; 4];
    let mut memory: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
    memory.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    // SAFETY: pseudo-handle is valid for this process, pointers refer to live correctly sized structs.
    unsafe {
        let h = GetCurrentProcess();
        let [creation, exit, kernel, user] = &mut times;
        if GetProcessTimes(h, creation, exit, kernel, user) == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if GetProcessMemoryInfo(h, &mut memory, memory.cb) == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    let ticks = |t: &FILETIME| ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64;
    Ok((
        (ticks(&times[2]) + ticks(&times[3])) as f64 / 10_000.0,
        memory.WorkingSetSize as u64,
        memory.PeakWorkingSetSize as u64,
    ))
}
#[cfg(not(windows))]
pub fn process_metrics() -> Result<(f64, u64, u64)> {
    Err("This benchmark requires Windows process counters; do not substitute fabricated CPU/RAM values".into())
}
