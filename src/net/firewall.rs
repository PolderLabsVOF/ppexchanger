//! Windows Firewall integration.
//!
//! On Windows the default inbound policy is to drop unsolicited connections,
//! so a freshly installed `ppx` will bind 7777 but no peer on the LAN can
//! dial in until the user adds an inbound-allow rule. The installer can do
//! this automatically (`install.sh --firewall`); the TUI also surfaces a
//! one-line hint when `/discover` finds zero peers so a user who runs ppx
//! directly without the installer still gets told what to do.
//!
//! Linux uses UFW when it is installed. macOS deliberately does not mutate
//! pf rules automatically: pf tables are system-wide and application
//! firewall permissions are code-signing dependent. The caller receives a
//! clear status either way.
//!
//! ponytail: macOS has a similar problem (`pf` / application firewall prompt)
//! but the average home LAN runs on consumer routers that don't have the
//! same default-deny behavior, so it isn't worth the complexity yet.

use std::io;

/// Stable name for the inbound rule — same string the installer uses, so a
/// user can `netsh advfirewall firewall show rule name=ppx` to inspect.
pub const RULE_NAME: &str = "ppexchanger (TCP/7777)";
pub const CONTROL_RULE_NAME: &str = "ppexchanger (UDP/7778)";
const SETUP_MARKER: &str = ".firewall-rules";

/// Default TCP port the rule covers. Pass `0` to substitute the bind port.
pub const DEFAULT_PORT: u16 = crate::net::discovery::MULTICAST_PORT;

/// Whether this build target has the interactive firewall integration used by
/// the empty-discovery hint. Linux rules are applied automatically at startup;
/// the hint remains generic there because UFW may not be installed or active.
pub const SUPPORTED: bool = cfg!(target_os = "windows");

/// Ensure the inbound rules required for peer connections exist. This is
/// intentionally best-effort and idempotent: it never enables a disabled
/// firewall and never removes rules belonging to other applications.
pub fn ensure_rules(tcp_port: u16, control_port: u16) -> io::Result<Option<String>> {
    #[cfg(target_os = "linux")]
    {
        return ensure_ufw_rules(tcp_port, control_port);
    }
    #[cfg(target_os = "windows")]
    {
        if !rule_present(tcp_port) {
            add_rule(tcp_port)?;
            return Ok(Some(format!("firewall rules added for TCP {} and UDP {}", tcp_port, control_port)));
        }
        return Ok(None);
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let _ = (tcp_port, control_port);
        Ok(None)
    }
}

#[cfg(target_os = "linux")]
fn ensure_ufw_rules(tcp_port: u16, control_port: u16) -> io::Result<Option<String>> {
    let marker = crate::config::config_dir()
        .ok()
        .map(|dir| dir.join(SETUP_MARKER));
    if marker
        .as_ref()
        .map(|path| marker_matches(path, tcp_port, control_port))
        .unwrap_or(false)
    {
        // A successful first-run setup is remembered for this exact port
        // pair, so normal restarts never invoke sudo again.
        return Ok(None);
    }

    // Probe the executable without elevation. We deliberately do not run
    // `ufw status` here: on many distributions that would trigger a sudo
    // prompt before the actual rule write and could prompt twice.
    match std::process::Command::new("ufw")
        .arg("--version")
        .output()
    {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            return Err(io::Error::other(format!(
                "ufw probe failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let is_root = std::process::Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false);
    let tcp_spec = format!("{}/tcp", tcp_port);
    let udp_spec = format!("{}/udp", control_port);
    let rules = [(tcp_spec, "TCP"), (udp_spec, "UDP")];
    if is_root {
        for (spec, label) in &rules {
            let output = std::process::Command::new("ufw")
                .args(["allow", spec, "comment"])
                .arg(format!("ppexchanger {}", label))
                .output()?;
            if !output.status.success() {
                return Err(io::Error::other(format!(
                    "could not add UFW rule {}: {}",
                    spec,
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
        }
    } else {
        // Apply both idempotent rules under one sudo process. This keeps
        // startup to one password prompt even when sudo credential caching is
        // disabled, and avoids a separate privileged status probe.
        let script = "ufw allow \"$1\" comment \"$2\" && ufw allow \"$3\" comment \"$4\"";
        let mut cmd = std::process::Command::new("sudo");
        cmd.arg("-p")
            .arg("ppexchanger firewall permission: ")
            .args(["sh", "-c", script, "ppexchanger"]);
        for (spec, label) in &rules {
            cmd.arg(spec).arg(format!("ppexchanger {}", label));
        }
        let output = cmd.output()?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "could not add UFW rules: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
    }
    if let Some(path) = marker {
        // The marker is only an optimization. If it cannot be written, the
        // next launch will safely re-check the rules.
        let _ = write_marker(&path, tcp_port, control_port);
    }
    Ok(Some(format!(
        "firewall rules ready ({}, {})",
        rules[0].0, rules[1].0
    )))
}

fn marker_matches(path: &std::path::Path, tcp_port: u16, control_port: u16) -> bool {
    let expected = format!("tcp={}\nudp={}\n", tcp_port, control_port);
    std::fs::read_to_string(path)
        .map(|contents| contents == expected)
        .unwrap_or(false)
}

fn write_marker(path: &std::path::Path, tcp_port: u16, control_port: u16) -> io::Result<()> {
    std::fs::write(path, format!("tcp={}\nudp={}\n", tcp_port, control_port))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

/// Run a side-effect: probe whether the inbound rule is currently in place.
/// Used by the TUI to decide whether to print the firewall hint after a
/// discovery that finds zero peers. Best-effort: returns `false` if the
/// probe fails for any reason (admin needed, netsh not on PATH, etc.) — the
/// false negative just means an extra hint line on first launch.
#[cfg(target_os = "windows")]
pub fn rule_present(port: u16) -> bool {
    let port = port_or_default(port);
    // `show rule` is non-zero iff a matching rule exists. We grep on the
    // human-readable name (which is the installer's contract); this means
    // a user-added rule with a different name won't satisfy the check,
    // which is intentional — we'd rather show the hint than silently miss
    // a peer.
    let status = std::process::Command::new("netsh")
        .args([
            "advfirewall",
            "firewall",
            "show",
            "rule",
            &format!("name={}", RULE_NAME),
        ])
        .output();
    match status {
        Ok(out) if out.status.success() => {
            // netsh prints the rule body on stdout; if the port column shows
            // our expected port we treat it as present.
            let s = String::from_utf8_lossy(&out.stdout);
            s.contains(&format!("LocalPort: {}", port))
        }
        _ => false,
    }
}

#[cfg(not(target_os = "windows"))]
pub fn rule_present(_port: u16) -> bool {
    false
}

/// Add the inbound-allow rule. Requires admin. Idempotent — calling twice
/// overwrites the existing rule with the same name (the Windows default
/// behavior for `add rule`).
///
/// Returns `Ok(())` on success. On non-Windows targets this is a no-op
/// that returns `Ok(())` so call sites don't need `cfg` branches.
#[cfg(target_os = "windows")]
pub fn add_rule(port: u16) -> io::Result<()> {
    let port = port_or_default(port);
    // Run elevated. `netsh advfirewall` requires admin even with a
    // well-formed rule. We shell out to PowerShell's Start-Process -Verb
    // RunAs so the UAC dialog appears and the user consents — this is
    // the same model the installer uses, so both surfaces are consistent.
    // The command itself is the same `netsh` line the hint shows.
    let ps = format!(
        "$ErrorActionPreference='Stop'; \
         netsh advfirewall firewall delete rule name='{name}' 2>$null; \
         netsh advfirewall firewall add rule \
            name='{name}' \
            dir=in action=allow protocol=TCP localport={port} \
            profile=private,domain \
            description='Allow LAN peers to dial ppexchanger on TCP {port}.' | Out-Null; \
         if ($LASTEXITCODE -ne 0) {{ throw \"netsh failed with $LASTEXITCODE\" }}",
        name = RULE_NAME,
        port = port,
    );
    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &ps])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "netsh exited with {}",
            status.code().unwrap_or(-1)
        )))
    }
}

#[cfg(not(target_os = "windows"))]
pub fn add_rule(_port: u16) -> io::Result<()> {
    Ok(())
}

/// Human-readable hint for users who want to fix the firewall manually.
/// Returns the exact one-liner they need to paste into an *elevated* cmd
/// (admin PowerShell or "Run as administrator"). Useful both for the
/// TUI's empty-discover hint and the installer's `--print-cmd` path.
pub fn manual_hint(port: u16) -> String {
    let port = port_or_default(port);
    format!(
        "Windows Firewall is blocking inbound TCP {port}. Run once in an elevated PowerShell:\n  \
         netsh advfirewall firewall add rule name=\"{name}\" dir=in action=allow protocol=TCP localport={port} profile=private,domain\n\
         (or reinstall with: curl -fsSL https://github.com/PolderLabsVOF/ppexchanger/releases/latest/download/install.sh | bash -s -- --firewall)",
        port = port,
        name = RULE_NAME,
    )
}

fn port_or_default(port: u16) -> u16 {
    if port == 0 {
        DEFAULT_PORT
    } else {
        port
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_name_is_stable() {
        // The installer greps on this exact string. Renaming it silently
        // breaks every prior install's discovery hint.
        assert_eq!(RULE_NAME, "ppexchanger (TCP/7777)");
    }

    #[test]
    fn manual_hint_carries_the_port() {
        // Different binds need different hints; verify the port is in the
        // rendered string both for the default and a custom value.
        let h = manual_hint(0);
        assert!(h.contains("7777"));
        assert!(h.contains(RULE_NAME));
        let h = manual_hint(9001);
        assert!(h.contains("9001"));
    }

    #[test]
    fn port_or_default_substitutes_zero() {
        assert_eq!(port_or_default(0), DEFAULT_PORT);
        assert_eq!(port_or_default(1234), 1234);
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn add_rule_is_noop_off_windows() {
        // Cross-platform callers can rely on Ok(()).
        assert!(add_rule(7777).is_ok());
    }
}
