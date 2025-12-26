use anyhow::Context;

use nix::{
    mount::{MsFlags, mount},
    sched::{CloneFlags, clone},
    sys::signal::Signal,
    unistd::Pid,
};

const STACK_SIZE: usize = 1024 * 1024;

fn child(command: &str, args: &[String]) -> anyhow::Result<()> {
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

    mount(
        Some("proc"),
        "/proc",
        None::<&str>,
        MsFlags::empty(),
        None::<&str>,
    )
    .context("failed to mount procfs")?;

    // execvp replaces the current process, so this only returns on error
    execvp(&cmd_cstring, &c_args).context("failed to execute command")?;

    // This line is never reached if execvp succeeds
    unreachable!()
}

pub fn run_in_container(command: &str, args: &[String]) -> anyhow::Result<()> {
    // clone flags
    let clone_flags = CloneFlags::CLONE_NEWPID;
    // allocate stack for the child process
    let mut stack = vec![0u8; STACK_SIZE];

    let child_pid = unsafe {
        clone(
            Box::new(move || {
                // This runs in the child process with PID 1 in the new namespace
                if let Err(e) = child(command, args) {
                    eprintln!("child process failed: {}", e);
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
