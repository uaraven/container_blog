use std::{
    fs::create_dir_all,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
};

use anyhow::Context;

use libc::{getegid, geteuid};
use nix::{
    mount::{MsFlags, mount},
    sched::{CloneFlags, clone},
    sys::signal::Signal,
    unistd::{Pid, chdir, close, pipe, pivot_root, read, write},
};

const STACK_SIZE: usize = 1024 * 1024;

fn child(command: &str, args: &[String]) -> anyhow::Result<()> {
    mount(
        None::<&str>,
        "/",
        None::<&str>,
        MsFlags::MS_REC | MsFlags::MS_PRIVATE,
        None::<&str>,
    )
    .context("private propagation for /")?;

    mount(
        Some("rootfs"),
        "rootfs",
        None::<&str>,
        MsFlags::MS_BIND | MsFlags::MS_REC,
        None::<&str>,
    )
    .context("bind mount rootfs")?;

    create_dir_all("rootfs/old_root").context("create old_root")?;
    pivot_root("rootfs", "rootfs/old_root").context("pivot_root")?;
    chdir("/").context("chdir to /")?;

    mount(
        Some("proc"),
        "proc",
        Some("proc"),
        MsFlags::empty(),
        None::<&str>,
    )
    .context("mount /proc")?;

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
