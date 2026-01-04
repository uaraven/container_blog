use std::{
    fs::{create_dir_all, remove_dir, remove_dir_all},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::Path,
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

fn recreate_dir<P: AsRef<Path>>(dir: P) -> anyhow::Result<()> {
    if dir.as_ref().exists() {
        std::fs::remove_dir_all(dir.as_ref())
            .with_context(|| format!("failed to remove {:?}", dir.as_ref()))?;
    }
    std::fs::create_dir_all(dir.as_ref())
        .with_context(|| format!("failed to create {:?}", dir.as_ref()))?;
    Ok(())
}

fn create_overlay_dirs(root: &str) -> anyhow::Result<(String, String, String, String)> {
    let lower_dirs = find_lower_layers(root)?;

    let upper_dir = format!("{}/upper", root);
    recreate_dir(&upper_dir)?;

    let lower = if lower_dirs.is_empty() {
        format!("{}/rootfs", root)
    } else {
        format!("{}/rootfs:{}", root, lower_dirs)
    };

    let workdir = Path::new(root).join("workdir");
    let rootfs = Path::new(root).join("mount");
    recreate_dir(&workdir)?;
    recreate_dir(&rootfs)?;

    Ok((
        lower,
        upper_dir,
        workdir.to_string_lossy().to_string(),
        rootfs.to_string_lossy().to_string(),
    ))
}

pub fn find_lower_layers(root: &str) -> anyhow::Result<String> {
    let mut names: Vec<String> = Vec::new();

    for entry in std::fs::read_dir(root).context("failed to read root directory")? {
        let entry = entry.context("failed to read directory entry")?;
        let file_type = entry.file_type().context("failed to get file type")?;
        if !file_type.is_dir() {
            continue;
        }
        let os_name = entry.file_name();
        if let Some(name) = os_name.to_str() {
            // match "layer" followed by exactly two digits
            let is_match = name.len() == 7
                && name.starts_with("layer")
                && name.chars().skip(5).take(2).all(|c| c.is_ascii_digit());
            if is_match {
                names.push(format!("{}/{}", root, name.to_string()));
            }
        }
    }

    names.sort();
    Ok(names.join(":"))
}

/// Create the container's filesystem.
/// See [fs readme](fs/readme.md) for details about directorylayout
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

    let (lower, upper, workdir, rootdir) = create_overlay_dirs(root)?;

    let rootfs = Path::new(&rootdir);

    let mount_opts = format!("lowerdir={},upperdir={},workdir={}", lower, upper, workdir);

    mount(
        Some("overlay"),
        rootfs,
        Some("overlay"),
        MsFlags::empty(),
        Some(mount_opts.as_str()),
    )
    .context("mount overlayfs")?;

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
    pivot_root(rootfs, &old_root).context("pivot_root")?;
    chdir("/").context("chdir to /")?;
    umount2("/.old_root", MntFlags::MNT_DETACH).context("umount old_root")?;
    let _ = remove_dir("/.old_root");

    Ok(())
}
