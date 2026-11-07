//! Process monitoring functionality
//!
//! This module handles monitoring and managing system processes, including
//! collecting process information and managing process lifecycle.

use super::{ProcessData, ProcessInfo, ProcessStaticInfo};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt::Debug;
use std::time::{SystemTime, UNIX_EPOCH};
use sysinfo::ProcessStatus;

fn os_string_vec_to_string_vec(v: &[OsString]) -> Vec<String> {
    v.iter()
        .map(|c| c.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
}

/// The app a launchd-adopted helper works for. macOS starts XPC services such as
/// WebKit's content, GPU and networking processes under launchd (PID 1), but
/// tracks the app responsible for each one. Only launchd's children are asked:
/// anything started from a terminal would otherwise be attributed to the
/// terminal. The answer is fixed for a process's lifetime, so it is cached.
#[cfg(target_os = "macos")]
fn owner_of(pid: u32, ppid: u32) -> Option<u32> {
    extern "C" {
        // Exported by libSystem since macOS 10.14; the same lookup the system
        // uses for privacy attribution.
        fn responsibility_get_pid_responsible_for_pid(pid: i32) -> i32;
    }
    if ppid != 1 {
        return None;
    }
    let pid = i32::try_from(pid).ok()?;
    // SAFETY: takes and returns plain integers; an unknown pid yields -1.
    let owner = unsafe { responsibility_get_pid_responsible_for_pid(pid) };
    (owner > 1 && owner != pid).then_some(owner as u32)
}

#[cfg(not(target_os = "macos"))]
fn owner_of(_pid: u32, _ppid: u32) -> Option<u32> {
    None
}

/// Monitors and manages system processes
#[derive(Debug)]
pub struct ProcessMonitor {
    /// Cache for static process information to avoid redundant allocations
    process_cache: HashMap<(u32, u64), ProcessStaticInfo>,
}

impl ProcessMonitor {
    /// Creates a new process monitor instance
    pub fn new() -> Self {
        Self {
            process_cache: HashMap::new(),
        }
    }

    /// Collects information about all running processes
    ///
    /// # Arguments
    ///
    /// * `sys` - System information provider
    ///
    /// # Returns
    ///
    /// A vector of process information, or an error string if collection failed
    pub fn collect_processes(&mut self, sys: &sysinfo::System) -> Result<Vec<ProcessInfo>, String> {
        // Keep only the identities in this capture. PIDs may be reused, and
        // retaining exited processes makes the cache grow for the app's lifetime.
        self.process_cache.retain(|&(pid, start_time), _| {
            sys.process(sysinfo::Pid::from_u32(pid))
                .is_some_and(|process| process.start_time() == start_time)
        });
        let current_time = Self::get_current_time()?;
        let processes_data = self.collect_process_data(sys, current_time);
        Ok(self.build_process_info(processes_data))
    }

    /// Attempts to kill a process
    ///
    /// # Arguments
    ///
    /// * `sys` - System information provider
    /// * `pid` - Process ID to kill
    ///
    /// # Returns
    ///
    /// Boolean indicating whether the process was successfully killed
    pub fn kill_process(sys: &sysinfo::System, pid: u32, start_time: u64) -> bool {
        // PIDs are reused; only signal the process that was actually shown.
        sys.process(sysinfo::Pid::from_u32(pid))
            .filter(|process| process.start_time() == start_time)
            .is_some_and(|process| process.kill())
    }

    /// Gets the current system time in seconds since UNIX epoch
    fn get_current_time() -> Result<u64, String> {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .map_err(|e| format!("Failed to get system time: {}", e))
    }

    /// Collects raw process data from the system
    fn collect_process_data(&self, sys: &sysinfo::System, current_time: u64) -> Vec<ProcessData> {
        sys.processes()
            .iter()
            .map(|(pid, process)| {
                let start_time = process.start_time();
                ProcessData {
                    pid: pid.as_u32(),
                    name: process.name().to_string_lossy().into_owned(),
                    cmd: os_string_vec_to_string_vec(process.cmd()),
                    user_id: process.user_id().map(|uid| uid.to_string()),
                    cpu_usage: process.cpu_usage(),
                    memory: process.memory(),
                    status: process.status(),
                    ppid: process.parent().map(|p| p.as_u32()),
                    environ: os_string_vec_to_string_vec(process.environ()),
                    root: process
                        .root()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    virtual_memory: process.virtual_memory(),
                    start_time,
                    run_time: if start_time > 0 {
                        current_time.saturating_sub(start_time)
                    } else {
                        0
                    },
                    disk_usage: process.disk_usage(),
                    session_id: process.session_id().map(|id| id.as_u32()),
                }
            })
            .collect()
    }

    /// Builds process information from raw process data
    fn build_process_info(&mut self, processes: Vec<ProcessData>) -> Vec<ProcessInfo> {
        processes
            .into_iter()
            .map(|data| {
                let cached_info = self
                    .process_cache
                    .entry((data.pid, data.start_time))
                    .or_insert_with(|| ProcessStaticInfo {
                        name: data.name.clone(),
                        command: data.cmd.join(" "),
                        user: data.user_id.unwrap_or_else(|| "-".to_string()),
                        owner: data.ppid.and_then(|ppid| owner_of(data.pid, ppid)),
                    });

                ProcessInfo {
                    gpu_usage: None,
                    ports: vec![],
                    pid: data.pid,
                    ppid: data.ppid.unwrap_or(0),
                    owner: cached_info.owner,
                    name: cached_info.name.clone(),
                    cpu_usage: data.cpu_usage,
                    memory_usage: data.memory,
                    status: Self::format_status(data.status),
                    user: cached_info.user.clone(),
                    command: cached_info.command.clone(),
                    threads: None,
                    environ: data.environ,
                    root: data.root,
                    virtual_memory: data.virtual_memory,
                    start_time: data.start_time,
                    run_time: data.run_time,
                    disk_usage: (data.disk_usage.read_bytes, data.disk_usage.written_bytes),
                    session_id: data.session_id,
                }
            })
            .collect()
    }

    /// Formats process status into a human-readable string
    pub fn format_status(status: ProcessStatus) -> String {
        match status {
            ProcessStatus::Run => "Running",
            ProcessStatus::Sleep => "Sleeping",
            ProcessStatus::Idle => "Idle",
            _ => "Unknown",
        }
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sysinfo::System;

    /// Tests creation of a new process monitor
    #[test]
    fn test_process_monitor_creation() {
        let monitor = ProcessMonitor::new();
        assert!(monitor.process_cache.is_empty());
    }

    /// Tests process collection functionality
    #[test]
    fn test_process_collection() {
        let mut monitor = ProcessMonitor::new();
        let mut sys = System::new();
        sys.refresh_all();

        let result = monitor.collect_processes(&sys);
        assert!(result.is_ok());
    }
    #[test]
    fn cache_discards_exited_and_reused_process_identities() {
        let mut sys = System::new();
        let pid = sysinfo::Pid::from_u32(std::process::id());
        sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
        let start_time = sys.process(pid).unwrap().start_time();
        let mut monitor = ProcessMonitor::new();
        monitor.collect_processes(&sys).unwrap();
        assert_eq!(monitor.process_cache.len(), 1);
        let stale = ProcessStaticInfo {
            name: "stale".into(),
            command: "stale command".into(),
            user: "stale user".into(),
            owner: None,
        };
        // A reused PID must not inherit the previous process's static metadata.
        monitor.process_cache.clear();
        monitor
            .process_cache
            .insert((pid.as_u32(), start_time.wrapping_add(1)), stale.clone());
        let processes = monitor.collect_processes(&sys).unwrap();
        assert_ne!(processes[0].name, "stale");
        assert_eq!(monitor.process_cache.len(), 1);
        assert!(monitor
            .process_cache
            .contains_key(&(pid.as_u32(), start_time)));

        // Simulate churn; retention must depend on live processes, not how
        // many process identities have ever passed through the monitor.
        for id in 1..=10000 {
            monitor.process_cache.insert((id, u64::MAX), stale.clone());
        }
        monitor.collect_processes(&sys).unwrap();
        assert_eq!(monitor.process_cache.len(), 1);
        monitor.collect_processes(&System::new()).unwrap();
        assert!(monitor.process_cache.is_empty());
    }
}
