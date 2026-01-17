use std::{
    net::Ipv4Addr,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
};

use anyhow::Context;

use caps::{CapSet, Capability};
use cidr::Ipv4Cidr;
use libc::{getegid, geteuid};
use nix::{
    sched::{CloneFlags, clone},
    sys::{
        signal::{SigSet, Signal, kill},
        signalfd::SignalFd,
        wait::{WaitPidFlag, WaitStatus, waitpid},
    },
    unistd::{ForkResult, Pid, close, fork, pipe, read, sethostname, write},
};

use crate::net;
use crate::{cgroups::Cgroup, fs};

const STACK_SIZE: usize = 1024 * 1024;

struct ContainerConfig {
    is_parent_root: bool,
    network_cidr: Ipv4Cidr,
    hostname: Option<String>,
    drop_caps: bool,
}

fn child(command: &str, args: &[String], config: &ContainerConfig) -> anyhow::Result<()> {
    if !config.is_parent_root {
        fs::create_overlay_dirs("fs")?;
    }
    fs::create_container_filesystem("fs")?;

    net::bring_up_container_net(&config.network_cidr, config.is_parent_root)?;

    if let Some(hostname) = &config.hostname {
        sethostname(hostname.as_str())?;
    }

    use nix::unistd::execve;
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

    // Build environment variables as CStrings: "KEY=VALUE"
    let mut c_env: Vec<CString> = Vec::new();
    for (key, value) in std::env::vars() {
        let updated_value = if key == "PATH" {
            // overwrite the PATH env variable to match alpine rootfs
            String::from("/bin:/sbin:/usr/bin:/usr/sbin")
        } else {
            value
        };
        let pair = format!("{}={}", key, updated_value);
        c_env.push(CString::new(pair).context("failed to convert env var to CString")?);
    }

    if config.drop_caps {
        drop_caps()?;
    }

    match unsafe { fork() }.context("failed to fork")? {
        ForkResult::Child => {
            // execve replaces the current process, so this only returns on error
            execve(&cmd_cstring, &c_args, &c_env).context("failed to execute command")?;

            // This line is never reached if execve succeeds
            unreachable!()
        }
        ForkResult::Parent { child } => {
            run_init(child)?;
        }
    }

    Ok(())
}

fn drop_caps() -> anyhow::Result<()> {
    let mut caps_drop = caps::all();
    caps_drop.remove(&Capability::CAP_CHOWN);

    for cap in caps_drop {
        caps::drop(None, CapSet::Bounding, cap)
            .context(format!("failed to drop bounding capability {}", cap))?;
    }

    nix::sys::prctl::set_no_new_privs()?;
    Ok(())
}

fn reap_zombies(child: Pid) {
    loop {
        match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(pid, status)) if pid == child => {
                println!("child exited with status {}", status);
                std::process::exit(status);
            }
            Ok(WaitStatus::Signaled(pid, sig, _)) if pid == child => {
                println!("child received signal {}", sig);
                std::process::exit(128 + sig as i32);
            }
            Ok(_) => break,
            Err(nix::errno::Errno::ECHILD) => break,
            Err(err) => {
                eprintln!("waitpid error: {}", err);
                std::process::exit(1);
            }
        }
    }
}

fn run_init(child: Pid) -> anyhow::Result<()> {
    let _ = nix::sys::prctl::set_child_subreaper(true);
    let mut signal_mask = SigSet::empty();
    signal_mask.add(Signal::SIGTERM);
    signal_mask.add(Signal::SIGINT);
    signal_mask.add(Signal::SIGQUIT);
    signal_mask.add(Signal::SIGCHLD);
    signal_mask.thread_block().context("signal therad_block")?;

    let signal_fd = SignalFd::new(&signal_mask)?;

    loop {
        let signal_info = signal_fd.read_signal()?.unwrap();
        let signal = Signal::try_from(signal_info.ssi_signo as i32).unwrap();
        match signal {
            Signal::SIGCHLD => {
                reap_zombies(child);
            }
            _ => {
                kill(child, signal)?;
            }
        }
    }
}

pub fn run_in_container(
    command: &str,
    args: &[String],
    cpu: &Option<String>,
    mem: &Option<String>,
    hostname: &Option<String>,
    drop_caps: bool,
) -> anyhow::Result<()> {
    // clone flags
    let clone_flags = CloneFlags::CLONE_NEWPID
        | CloneFlags::CLONE_NEWUSER
        | CloneFlags::CLONE_NEWNS
        | CloneFlags::CLONE_NEWNET
        | CloneFlags::CLONE_NEWUTS;
    // allocate stack for the child process
    let mut stack = vec![0u8; STACK_SIZE];

    let container_net_cidr =
        Ipv4Cidr::new(Ipv4Addr::new(192, 168, 200, 0), 24).context("invalid CIDR")?;

    let uid = unsafe { geteuid() };
    let gid = unsafe { getegid() };

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

                let config = ContainerConfig {
                    is_parent_root: uid == 0,
                    network_cidr: container_net_cidr,
                    hostname: hostname.clone(),
                    drop_caps: drop_caps,
                };
                // This runs in the child process with PID 1 in the new namespace
                if let Err(e) = child(command, args, &config) {
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

    write_proc_file(child_pid, "uid_map", &format!("0 {} 1\n", uid))?;
    write_proc_file(child_pid, "setgroups", "deny\n")?;
    write_proc_file(child_pid, "gid_map", &format!("0 {} 1\n", gid))?;

    // keep variable here, so if we use cgroup, it will be dropped automatically
    // when run_in_container finishes
    let mut _cgroup: Option<Cgroup> = None;

    if uid == 0 {
        fs::create_overlay_dirs("fs")?;

        net::setup_network_host(&container_net_cidr)?;
        net::move_into_container(child_pid)?;

        let cg = Cgroup::new(cpu, mem)?;
        cg.add_process(child_pid.as_raw())?;
        _cgroup = Some(cg);
    }

    write(&write_fd, b"1")?;
    close(write_fd)?;

    println!("started child with PID={}", child_pid);
    let _ = wait_for_child(child_pid);

    if uid == 0 {
        net::cleanup_network()?;
    }

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
