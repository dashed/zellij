//! Raise the server's open-file soft limit (RLIMIT_NOFILE) at startup.
//!
//! macOS gives processes launched from the GUI a soft limit of 256 open files.
//! A zellij server holds roughly a dozen descriptors per tab (pty masters plus
//! the directories each plugin instance preopens), so a busy session can run
//! out: new panes and tabs then fail to open and clients cannot attach.
//!
//! The server raises its own soft limit when it starts. Programs started in
//! panes get the original limit back (`restore_for_child`), so they run with
//! the same limit they would have outside zellij; some older programs still
//! use select(2), which misbehaves on descriptors above FD_SETSIZE.
//!
//! Other processes the server starts (background commands, the desktop app)
//! inherit the raised limit. If one of them starts another zellij server, that
//! server finds nothing to raise, so the original limit also travels in
//! `ORIGINAL_LIMIT_ENV` for it to restore in its own panes.

use std::sync::OnceLock;

/// Upper bound for the raised soft limit. macOS rejects RLIMIT_NOFILE values
/// above OPEN_MAX (10240); on other platforms it is still plenty.
const TARGET_SOFT_LIMIT: libc::rlim_t = 10240;

/// `<original soft limit>:<raised soft limit>`, set by the server that raised.
const ORIGINAL_LIMIT_ENV: &str = "ZELLIJ_NOFILE_BEFORE_RAISE";

/// The limit pane programs should get, recorded only if it differs from ours.
static ORIGINAL_LIMIT: OnceLock<libc::rlimit> = OnceLock::new();

fn current_limit() -> Option<libc::rlimit> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit only writes into the struct we pass it.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0 {
        Some(limit)
    } else {
        None
    }
}

/// The soft limit to raise to, or `None` when the current one is already high
/// enough. Never lowers the limit and never exceeds the hard limit.
fn raised_soft_limit(soft: libc::rlim_t, hard: libc::rlim_t) -> Option<libc::rlim_t> {
    let target = TARGET_SOFT_LIMIT.min(hard);
    if target > soft {
        Some(target)
    } else {
        None
    }
}

/// The original soft limit recorded by an ancestor zellij server, if our
/// current limit is still exactly the one it raised to. Any other current
/// value means someone changed the limit since, and that choice wins.
fn inherited_original_soft(env_value: &str, current_soft: libc::rlim_t) -> Option<libc::rlim_t> {
    let (original, raised) = env_value.split_once(':')?;
    let original: libc::rlim_t = original.parse().ok()?;
    let raised: libc::rlim_t = raised.parse().ok()?;
    if raised == current_soft && original < raised {
        Some(original)
    } else {
        None
    }
}

/// Raise this process's open-file soft limit. Call once, early in the server,
/// while it is still single-threaded (it sets an environment variable).
pub fn raise() {
    let Some(current) = current_limit() else {
        log::warn!(
            "Could not read the open-file limit: {}",
            std::io::Error::last_os_error()
        );
        return;
    };
    if let Some(original_soft) = std::env::var(ORIGINAL_LIMIT_ENV)
        .ok()
        .and_then(|value| inherited_original_soft(&value, current.rlim_cur))
    {
        // Started (indirectly) by a zellij server that already raised the limit.
        let _ = ORIGINAL_LIMIT.set(libc::rlimit {
            rlim_cur: original_soft,
            rlim_max: current.rlim_max,
        });
        return;
    }
    let Some(new_soft) = raised_soft_limit(current.rlim_cur, current.rlim_max) else {
        return;
    };
    let raised = libc::rlimit {
        rlim_cur: new_soft,
        rlim_max: current.rlim_max,
    };
    // SAFETY: setrlimit only reads the struct we pass it.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raised) } != 0 {
        log::warn!(
            "Could not raise the open-file limit from {} to {}: {}",
            current.rlim_cur,
            new_soft,
            std::io::Error::last_os_error()
        );
        return;
    }
    let _ = ORIGINAL_LIMIT.set(current);
    std::env::set_var(
        ORIGINAL_LIMIT_ENV,
        format!("{}:{}", current.rlim_cur, new_soft),
    );
    log::info!(
        "Raised the open-file limit from {} to {}",
        current.rlim_cur,
        new_soft
    );
}

/// Give a child process the open-file limit the server started with.
///
/// Runs between fork and exec (from `pre_exec`), so it must stay
/// async-signal-safe: one atomic read and one syscall, no allocation.
pub fn restore_for_child() {
    if let Some(original) = ORIGINAL_LIMIT.get() {
        // SAFETY: setrlimit only reads the struct; lowering a soft limit is
        // always permitted. Failure leaves the raised limit, which is harmless.
        unsafe {
            libc::setrlimit(libc::RLIMIT_NOFILE, original);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    #[test]
    fn raised_soft_limit_never_lowers_and_respects_the_hard_limit() {
        let unlimited = libc::RLIM_INFINITY;
        assert_eq!(raised_soft_limit(256, unlimited), Some(10240));
        assert_eq!(raised_soft_limit(256, 1024), Some(1024));
        assert_eq!(raised_soft_limit(256, 256), None);
        assert_eq!(raised_soft_limit(10240, unlimited), None);
        assert_eq!(raised_soft_limit(65536, unlimited), None);
    }

    #[test]
    fn inherited_original_is_trusted_only_while_the_raised_limit_is_unchanged() {
        assert_eq!(inherited_original_soft("256:10240", 10240), Some(256));
        // the limit was changed after the ancestor raised it: keep that choice
        assert_eq!(inherited_original_soft("256:10240", 4096), None);
        assert_eq!(inherited_original_soft("256:10240", 256), None);
        // malformed or nonsensical values are ignored
        assert_eq!(inherited_original_soft("10240:256", 256), None);
        assert_eq!(inherited_original_soft("256", 10240), None);
        assert_eq!(inherited_original_soft("x:10240", 10240), None);
        assert_eq!(inherited_original_soft("", 10240), None);
    }

    const PROBE_ENV: &str = "ZELLIJ_FD_LIMIT_PROBE";
    const PROBE_TEST: &str = "fd_limit::tests::raise_and_restore_probe";

    fn pane_program_limit() -> String {
        let output = unsafe {
            Command::new("sh")
                .args(["-c", "ulimit -n"])
                .pre_exec(|| {
                    restore_for_child();
                    Ok(())
                })
                .output()
                .unwrap()
        };
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// Runs for real only inside the child processes started by the two tests
    /// below, so the process-wide limit changes cannot leak into other tests.
    #[test]
    fn raise_and_restore_probe() {
        let Some(mode) = std::env::var_os(PROBE_ENV) else {
            return;
        };
        let before = current_limit().unwrap();
        raise();
        let after = current_limit().unwrap();
        if mode == "fresh" {
            assert_eq!(before.rlim_cur, 256);
            let expected = raised_soft_limit(256, before.rlim_max).unwrap();
            assert_eq!(after.rlim_cur, expected, "server soft limit was not raised");
            assert_eq!(
                std::env::var(ORIGINAL_LIMIT_ENV).unwrap(),
                format!("256:{expected}"),
                "original limit not recorded for descendant servers"
            );
        } else {
            assert_eq!(
                after.rlim_cur, before.rlim_cur,
                "already-raised limit changed"
            );
        }
        assert_eq!(
            pane_program_limit(),
            "256",
            "pane program did not get the original limit back"
        );
    }

    /// Re-run this test binary as a pretend server with the given soft limit.
    fn run_probe(mode: &str, soft: libc::rlim_t, inherited: Option<String>) {
        let hard = current_limit().unwrap().rlim_max;
        let mut probe = Command::new(std::env::current_exe().unwrap());
        probe
            .args(["--exact", PROBE_TEST, "--test-threads=1"])
            .env(PROBE_ENV, mode)
            .env_remove(ORIGINAL_LIMIT_ENV);
        if let Some(value) = inherited {
            probe.env(ORIGINAL_LIMIT_ENV, value);
        }
        let status = unsafe {
            probe
                .pre_exec(move || {
                    let limit = libc::rlimit {
                        rlim_cur: soft,
                        rlim_max: hard,
                    };
                    if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                })
                .status()
                .unwrap()
        };
        assert!(status.success(), "{mode} probe failed: {status}");
    }

    #[test]
    fn server_raises_limit_and_pane_programs_get_the_original_back() {
        let hard = current_limit().unwrap().rlim_max;
        if raised_soft_limit(256, hard).is_none() {
            return; // hard limit too low to raise anything
        }
        // Like a macOS GUI launch.
        run_probe("fresh", 256, None);
    }

    #[test]
    fn server_started_by_a_raised_process_still_restores_the_original() {
        let hard = current_limit().unwrap().rlim_max;
        let Some(raised) = raised_soft_limit(256, hard) else {
            return; // hard limit too low to raise anything
        };
        // Like a server started by the desktop app or a background command of
        // another server: it inherits the raised limit and the variable.
        run_probe("inherited", raised, Some(format!("256:{raised}")));
    }
}
