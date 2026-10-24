//! The Windows API calls lazymsbuild needs, behind safe wrappers.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::ptr;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Globalization::{GetACP, GetOEMCP, MultiByteToWideChar};
use windows_sys::Win32::Storage::FileSystem::GetLongPathNameW;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
    TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

pub fn ansi_code_page() -> u32 {
    // SAFETY: no arguments, no preconditions.
    unsafe { GetACP() }
}

pub fn oem_code_page() -> u32 {
    // SAFETY: no arguments, no preconditions.
    unsafe { GetOEMCP() }
}

/// Decodes `bytes` from `code_page`. Undecodable bytes become U+FFFD; nothing is dropped.
pub fn multi_byte_to_string(code_page: u32, bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    let Ok(len) = i32::try_from(bytes.len()) else {
        return String::from_utf8_lossy(bytes).into_owned();
    };
    // SAFETY: a null output buffer with size 0 asks for the required length.
    let needed = unsafe { MultiByteToWideChar(code_page, 0, bytes.as_ptr(), len, ptr::null_mut(), 0) };
    if needed <= 0 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut wide = vec![0u16; needed as usize];
    // SAFETY: `wide` has room for `needed` UTF-16 units, as the first call reported.
    let written = unsafe { MultiByteToWideChar(code_page, 0, bytes.as_ptr(), len, wide.as_mut_ptr(), needed) };
    if written <= 0 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    wide.truncate(written as usize);
    String::from_utf16_lossy(&wide)
}

/// A Job Object that kills every process in it when it is closed or terminated, so a cancelled
/// build leaves no MSBuild worker node, cl.exe or mspdbsrv.exe behind.
pub struct Job(HANDLE);

// SAFETY: a job handle can be used from any thread.
unsafe impl Send for Job {}
// SAFETY: the operations used (assign, terminate) are thread-safe in the Windows API.
unsafe impl Sync for Job {}

impl Job {
    pub fn new() -> io::Result<Job> {
        // SAFETY: null attributes and name create an anonymous job with default security.
        let handle = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Job(handle);
        // SAFETY: an all-zero JOBOBJECT_EXTENDED_LIMIT_INFORMATION is a valid "no limits" value.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `info` is the structure JobObjectExtendedLimitInformation expects, with its size.
        let ok = unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    /// Puts a process (and so every process it starts afterwards) into the job.
    pub fn assign(&self, process: HANDLE) -> io::Result<()> {
        // SAFETY: both handles are valid; the caller owns `process` for the duration of the call.
        if unsafe { AssignProcessToJobObject(self.0, process) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Ends every process in the job.
    pub fn terminate(&self) -> io::Result<()> {
        // SAFETY: the job handle is valid until drop.
        if unsafe { TerminateJobObject(self.0, 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: the handle is owned by this value and closed once.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// Resumes every thread of a process started with `CREATE_SUSPENDED`.
pub fn resume_process(pid: u32) -> io::Result<()> {
    // SAFETY: TH32CS_SNAPTHREAD with process id 0 snapshots all threads.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut resumed = 0;
    // SAFETY: dwSize must be set before the first call; the rest is filled in by the API.
    let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
    entry.dwSize = size_of::<THREADENTRY32>() as u32;
    // SAFETY: `snapshot` is valid and `entry` is initialized as the API requires.
    let mut more = unsafe { Thread32First(snapshot, &mut entry) } != 0;
    while more {
        if entry.th32OwnerProcessID == pid {
            // SAFETY: plain OpenThread/ResumeThread/CloseHandle on a thread id from the snapshot.
            unsafe {
                let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                if !thread.is_null() {
                    if ResumeThread(thread) != u32::MAX {
                        resumed += 1;
                    }
                    CloseHandle(thread);
                }
            }
        }
        // SAFETY: as above.
        more = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
    }
    // SAFETY: the snapshot handle is owned here.
    unsafe {
        CloseHandle(snapshot);
    }
    if resumed == 0 {
        return Err(io::Error::other(format!("no thread of process {pid} could be resumed")));
    }
    Ok(())
}

/// Opens a file with its default application (the Explorer "open" verb).
pub fn shell_open(path: &Path) -> io::Result<()> {
    let file = wide(path.as_os_str());
    let verb = wide(OsStr::new("open"));
    // SAFETY: both strings are NUL-terminated UTF-16; the other pointers may be null.
    let result = unsafe {
        ShellExecuteW(ptr::null_mut(), verb.as_ptr(), file.as_ptr(), ptr::null(), ptr::null(), SW_SHOWNORMAL)
    };
    // ShellExecuteW reports success with a value greater than 32.
    if result as isize <= 32 {
        return Err(io::Error::other(format!("ShellExecuteW failed ({})", result as isize)));
    }
    Ok(())
}

/// `path` with its 8.3 short names (`RUNNER~1`) written in full; `None` when it does not exist.
pub fn long_path_name(path: &Path) -> Option<PathBuf> {
    path_name(path, GetLongPathNameW)
}

/// `path` with 8.3 short names where the volume has them; `None` when it does not exist.
#[cfg(test)]
pub fn short_path_name(path: &Path) -> Option<PathBuf> {
    path_name(path, windows_sys::Win32::Storage::FileSystem::GetShortPathNameW)
}

fn path_name(path: &Path, convert: unsafe extern "system" fn(*const u16, *mut u16, u32) -> u32) -> Option<PathBuf> {
    let input = wide(path.as_os_str());
    let mut buffer = vec![0u16; 260];
    loop {
        // SAFETY: `input` is NUL-terminated and `buffer` has room for `buffer.len()` units.
        let len = unsafe { convert(input.as_ptr(), buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
        match len {
            0 => return None,
            // Too small: `len` is the size needed, with the NUL.
            len if len > buffer.len() => buffer.resize(len, 0),
            len => return Some(PathBuf::from(OsString::from_wide(&buffer[..len]))),
        }
    }
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}
