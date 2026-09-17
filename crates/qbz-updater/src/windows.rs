//! The MSI owns its files. Stage verified bytes and let Windows Installer do
//! the upgrade after the current application exits; never overwrite DLLs here.
use crate::Result;
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

pub fn is_msi_install(exe: &Path) -> bool {
    let key = wide("Software\\blitzfc\\QBZ");
    let name = wide("InstallDir");
    let mut size = 0u32;
    // This per-user key is authored by the fixed MainExe WiX component.
    unsafe {
        if RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        ) != 0
            || size > 65536
        {
            return false;
        }
        let mut bytes = vec![0u16; size as usize / 2];
        if RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            bytes.as_mut_ptr().cast(),
            &mut size,
        ) != 0
        {
            return false;
        }
        let end = bytes.iter().position(|c| *c == 0).unwrap_or(bytes.len());
        let Ok(dir) = String::from_utf16(&bytes[..end]) else {
            return false;
        };
        let registered = PathBuf::from(dir).join("qbz.exe").canonicalize();
        matches!((registered,exe.canonicalize()),(Ok(a),Ok(b)) if a==b)
    }
}

pub fn stage_msi(exe: &Path, stage: tempfile::TempDir, payload: &Path) -> Result<PathBuf> {
    if !is_msi_install(exe) {
        return Err("The installation is no longer owned by the QBZ MSI".into());
    }
    let mut input = std::fs::File::open(payload).map_err(|e| e.to_string())?;
    let mut magic = [0u8; 8];
    input.read_exact(&mut magic).map_err(|e| e.to_string())?;
    if magic != [0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1] {
        return Err("The signed package is not a Windows Installer database".into());
    }
    drop(input);
    let mut input = std::fs::File::open(payload).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = input.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    drop(input);
    let digest = format!("{:x}", hash.finalize());
    let package = stage.path().join("update.msi");
    std::fs::rename(payload, &package).map_err(|e| e.to_string())?;
    let script = stage.path().join("install.ps1");
    let ready = stage.path().join("ready");
    std::fs::write(&script, include_str!("install-msi.ps1")).map_err(|e| e.to_string())?;
    let powershell =
        PathBuf::from(std::env::var_os("SystemRoot").ok_or("SystemRoot is unavailable")?)
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let mut child = Command::new(powershell)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&script)
        .arg("-ParentPid")
        .arg(std::process::id().to_string())
        .arg("-AppPath")
        .arg(exe)
        .arg("-Package")
        .arg(&package)
        .arg("-Sha256")
        .arg(digest)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x08000000)
        .spawn()
        .map_err(|e| format!("Could not start Windows Installer helper: {e}"))?;
    let start = Instant::now();
    loop {
        if ready.is_file() {
            return Ok(stage.keep());
        }
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            return Err(format!(
                "Windows Installer helper exited before becoming ready: {status}"
            ));
        }
        if start.elapsed() > Duration::from_secs(15) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Windows Installer helper did not become ready".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
