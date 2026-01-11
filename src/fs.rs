use anyhow::{self, Context};
use nix::{
    mount::{MntFlags, MsFlags, mount, umount2},
    unistd::{chdir, pivot_root},
};
use std::{
    fs::{create_dir_all, remove_dir, remove_dir_all},
    path::Path,
};

fn recreate_dir<P: AsRef<Path>>(dir: P) -> anyhow::Result<()> {
    if dir.as_ref().exists() {
        std::fs::remove_dir_all(dir.as_ref())
            .with_context(|| format!("failed to remove {:?}", dir.as_ref()))?;
    }
    std::fs::create_dir_all(dir.as_ref())
        .with_context(|| format!("failed to create {:?}", dir.as_ref()))?;
    Ok(())
}

fn get_overlay_dirs(root: &str) -> anyhow::Result<(String, String, String, String)> {
    let lower_dirs = find_lower_layers(root)?;
    let upper_dir = format!("{}/upper", root);

    let lower = if lower_dirs.is_empty() {
        format!("{}/rootfs", root)
    } else {
        format!("{}/rootfs:{}", root, lower_dirs)
    };

    let workdir = Path::new(root).join("workdir");
    let rootfs = Path::new(root).join("mount");

    Ok((
        lower,
        upper_dir,
        workdir.to_string_lossy().to_string(),
        rootfs.to_string_lossy().to_string(),
    ))
}

pub(crate) fn create_overlay_dirs(root: &str) -> anyhow::Result<()> {
    let upper_dir = format!("{}/upper", root);
    recreate_dir(&upper_dir)?;

    let workdir = Path::new(root).join("workdir");
    let rootfs = Path::new(root).join("mount");
    recreate_dir(&workdir)?;
    recreate_dir(&rootfs)?;

    Ok(())
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
                names.push(format!("{}/{}", root, name));
            }
        }
    }

    names.sort();
    Ok(names.join(":"))
}

/// Create the container's filesystem.
/// See [fs readme](fs/readme.md) for details about directory layout
pub(crate) fn create_container_filesystem(root: &str) -> anyhow::Result<()> {
    // change the root fs propagation to private
    mount(
        None::<&str>,
        "/",
        None::<&str>,
        MsFlags::MS_REC | MsFlags::MS_PRIVATE,
        None::<&str>,
    )
    .context("private propagation for /")?;

    let (lower, upper, workdir, rootdir) = get_overlay_dirs(root)?;

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
