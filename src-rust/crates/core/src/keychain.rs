//! OS keychain integration for secure API key storage.
//!
//! Uses the native keychain on each platform:
//!   - macOS:  Keychain via `security` CLI (no extra deps)
//!   - Linux:  libsecret via `secret-tool` CLI
//!   - Windows: falls back to config file (TODO: use wincred)
//!
//! Keys are stored under service="uppli-code" with account=<provider>.

use std::process::Command;
use tracing::debug;

const SERVICE: &str = "uppli-code";

/// Store an API key in the OS keychain.
///
/// Returns `true` if successfully stored, `false` if keychain is unavailable.
pub fn store_key(provider: &str, key: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        store_macos(provider, key)
    }
    #[cfg(target_os = "linux")]
    {
        store_linux(provider, key)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (provider, key);
        false
    }
}

/// Retrieve an API key from the OS keychain.
///
/// Returns `None` if not found or keychain is unavailable.
pub fn get_key(provider: &str) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        get_macos(provider)
    }
    #[cfg(target_os = "linux")]
    {
        get_linux(provider)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = provider;
        None
    }
}

/// Delete an API key from the OS keychain.
pub fn delete_key(provider: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        delete_macos(provider)
    }
    #[cfg(target_os = "linux")]
    {
        delete_linux(provider)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = provider;
        false
    }
}

/// Check if the OS keychain is available.
pub fn is_available() -> bool {
    #[cfg(target_os = "macos")]
    {
        Command::new("security").arg("--help").output().is_ok()
    }
    #[cfg(target_os = "linux")]
    {
        Command::new("secret-tool")
            .arg("--version")
            .output()
            .is_ok()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        false
    }
}

// ---------------------------------------------------------------------------
// macOS: Keychain via `security` CLI
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn store_macos(provider: &str, key: &str) -> bool {
    // Delete existing entry first (update = delete + add).
    let _ = Command::new("security")
        .args(["delete-generic-password", "-s", SERVICE, "-a", provider])
        .output();

    let result = Command::new("security")
        .args([
            "add-generic-password",
            "-s",
            SERVICE,
            "-a",
            provider,
            "-w",
            key,
            "-U", // update if exists
        ])
        .output();

    match result {
        Ok(output) => {
            if output.status.success() {
                debug!(provider, "API key stored in macOS Keychain");
                true
            } else {
                debug!(
                    provider,
                    stderr = String::from_utf8_lossy(&output.stderr).as_ref(),
                    "Failed to store in Keychain"
                );
                false
            }
        }
        Err(e) => {
            debug!(error = %e, "security command not available");
            false
        }
    }
}

#[cfg(target_os = "macos")]
fn get_macos(provider: &str) -> Option<String> {
    let output = match Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            SERVICE,
            "-a",
            provider,
            "-w", // print password only
        ])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            debug!(provider, error = %e, "Keychain read failed — security command unavailable");
            return None;
        }
    };

    if output.status.success() {
        let key = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !key.is_empty() {
            return Some(key);
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn delete_macos(provider: &str) -> bool {
    Command::new("security")
        .args(["delete-generic-password", "-s", SERVICE, "-a", provider])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Linux: libsecret via `secret-tool` CLI
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn store_linux(provider: &str, key: &str) -> bool {
    use std::io::Write;

    let mut child = match Command::new("secret-tool")
        .args([
            "store",
            "--label",
            &format!("Uppli Code API key ({})", provider),
            "service",
            SERVICE,
            "provider",
            provider,
        ])
        .stdin(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            debug!(error = %e, "secret-tool not available");
            return false;
        }
    };

    if let Some(ref mut stdin) = child.stdin {
        if let Err(e) = stdin.write_all(key.as_bytes()) {
            debug!(error = %e, "Failed to write key to secret-tool stdin");
            return false;
        }
    }
    // Drop stdin explicitly so secret-tool sees EOF before we wait.
    drop(child.stdin.take());

    child.wait().map(|s| s.success()).unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn get_linux(provider: &str) -> Option<String> {
    let output = match Command::new("secret-tool")
        .args(["lookup", "service", SERVICE, "provider", provider])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            debug!(provider, error = %e, "Keychain read failed — secret-tool unavailable");
            return None;
        }
    };

    if output.status.success() {
        let key = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !key.is_empty() {
            return Some(key);
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn delete_linux(provider: &str) -> bool {
    Command::new("secret-tool")
        .args(["clear", "service", SERVICE, "provider", provider])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_available_does_not_panic() {
        // Just verify it doesn't crash — the result depends on the platform.
        let _ = is_available();
    }
}
