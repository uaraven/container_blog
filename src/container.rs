use std::{
    fs::{create_dir_all, remove_dir, remove_dir_all},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::Path,
    process::Command,
};

use anyhow::Context;

use libc::{getegid, geteuid};
use nix::{
    mount::{MntFlags, MsFlags, mount, umount2},
    sched::{CloneFlags, clone},
    sys::signal::Signal,
    unistd::{Pid, chdir, close, pipe, pivot_root, read, write},
};

const STACK_SIZE: usize = 1024 * 1024;

fn child(command: &str, args: &[String]) -> anyhow::Result<()> {
    create_container_filesystem("fs")?;

    use nix::unistd::execvp;
    use std::ffi::CString;

    // Convert command to CString
    let cmd_cstring = CString::new(command).context("failed to convert command to CString")?;

    // Convert arguments to CStrings
    // The first argument should be the program name itself
    let mut c_args: Vec<CString> = Vec::new();
    c_args.push(cmd_cstring.clone());

    for arg in args {
        c_args.push(CString::new(arg.as_str()).context("failed to convert argument to CString")?);
    }

    // execvp replaces the current process, so this only returns on error
    execvp(&cmd_cstring, &c_args).context("failed to execute command")?;

    // This line is never reached if execvp succeeds
    unreachable!()
}

pub fn run_in_container(command: &str, args: &[String]) -> anyhow::Result<()> {
    // clone flags
    let clone_flags =
        CloneFlags::CLONE_NEWPID | CloneFlags::CLONE_NEWUSER | CloneFlags::CLONE_NEWNS;
    // allocate stack for the child process
    let mut stack = vec![0u8; STACK_SIZE];

    let (read_fd, write_fd) = pipe()?;

    // convert to raw FD - I can't figure out how to trick borrow checked into allowing copying OwnedFd into the child
    let child_read_fd = read_fd.as_raw_fd();
    let child_write_fd = write_fd.as_raw_fd();

    let child_pid = unsafe {
        clone(
            Box::new(move || {
                // restore OwnedFd from raw FD
                let read_fd = OwnedFd::from_raw_fd(child_read_fd);
                let write_fd = OwnedFd::from_raw_fd(child_write_fd);

                // close writing part - we don't need it
                if let Err(e) = close(write_fd) {
                    eprint!("failed to close pipe {}", e);
                    return 1;
                }

                // wait for the parent
                let mut buf = [0u8];
                if let Err(e) = read(read_fd, &mut buf) {
                    eprint!("failed to sync with parent {}", e);
                    return 1;
                }

                // This runs in the child process with PID 1 in the new namespace
                if let Err(e) = child(command, args) {
                    eprintln!("child process failed: {:#}", e);
                    return 1;
                };
                return 0;
            }),
            &mut stack,
            clone_flags,
            Some(Signal::SIGCHLD as i32),
        )
    }
    .context("Failed to clone process")?;

    close(read_fd)?;

    let uid = unsafe { geteuid() };
    let gid = unsafe { getegid() };

    write_proc_file(child_pid, "uid_map", &format!("0 {} 1\n", uid))?;
    write_proc_file(child_pid, "setgroups", "deny\n")?;
    write_proc_file(child_pid, "gid_map", &format!("0 {} 1\n", gid))?;

    write(&write_fd, b"1")?;
    close(write_fd)?;

    println!("started child with PID={}", child_pid);
    let _ = wait_for_child(child_pid);

    Ok(())
}

fn wait_for_child(pid: Pid) -> anyhow::Result<i32> {
    use nix::sys::wait::{WaitStatus, waitpid};

    let result = match waitpid(pid, None).context("Failed to wait for child process")? {
        WaitStatus::Exited(_, code) => Ok(code),
        WaitStatus::Signaled(_, signal, _) => Ok(128 + signal as i32),
        _ => Ok(1),
    };

    result
}

fn write_proc_file(child_pid: Pid, file_name: &str, data: &str) -> anyhow::Result<()> {
    let path = format!("/proc/{}/{}", child_pid, file_name);
    std::fs::write(&path, data).with_context(|| format!("failed to write to {}", path))?;
    Ok(())
}

/// Create the container's filesystem.
/// Expects the root directory to exist and contain layout:
///  - rootfs/
///  - layerXX/
///  - upper/
///  - work/
///  - merged/
///
/// See [fs readme](fs/readme.md) for details
fn create_container_filesystem(root: &str) -> anyhow::Result<()> {
    // change the root fs propagation to private
    mount(
        None::<&str>,
        "/",
        None::<&str>,
        MsFlags::MS_REC | MsFlags::MS_PRIVATE,
        None::<&str>,
    )
    .context("private propagation for /")?;

    let rootfs = Path::new(root).join("rootfs");

    mount(
        Some(&rootfs),
        &rootfs,
        None::<&str>,
        MsFlags::MS_BIND | MsFlags::MS_REC,
        None::<&str>,
    )
    .context("bind mount rootfs")?;

    let proc = rootfs.join("proc");
    mount(
        Some("proc"),
        &proc,
        Some("proc"),
        MsFlags::empty(),
        None::<&str>,
    )
    .context("mount /proc")?;

    // prepare for pivot_root
    let old_root = rootfs.join(".old_root");
    if old_root.exists() {
        remove_dir_all(&old_root).context("remove old_root")?;
    }
    create_dir_all(&old_root).context("create old_root")?;

    // pivot_root and unmount old_root
    pivot_root(&rootfs, &old_root).context("pivot_root")?;
    chdir("/").context("chdir to /")?;
    umount2("/.old_root", MntFlags::MNT_DETACH).context("umount old_root")?;
    let _ = remove_dir("/.old_root");

    Ok(())
}

/// Execute a command with arguments, wait for its termination,
/// and return `anyhow::Result<()>`.
///
/// - `cmd`: the program to execute (e.g., "ls")
/// - `args`: the arguments to pass to the program (e.g., &["-la"])
pub fn execute_command(cmd: &str, args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new(cmd)
        .args(args)
        .status()
        .with_context(|| format!("failed to spawn command: {} {:?}", cmd, args))?;

    if status.success() {
        Ok(())
    } else {
        // Include exit code if available for better diagnostics
        match status.code() {
            Some(code) => anyhow::bail!(
                "command '{}' {:?} exited with non-zero status: {}",
                cmd,
                args,
                code
            ),
            None => anyhow::bail!("command '{}' {:?} terminated by signal", cmd, args),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_successfully() {
        // This should succeed on Unix-like systems
        execute_command("true", &[]).expect("command should succeed");
    }

    #[test]
    fn fails_as_expected() {
        // This should fail on Unix-like systems
        let err = execute_command("false", &[]).unwrap_err();
        assert!(
            format!("{err}").contains("non-zero"),
            "unexpected error: {err}"
        );
    }
}
