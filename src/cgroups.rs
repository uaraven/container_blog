use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

const CGROUP_PATH: &str = "/sys/fs/cgroup/";

/// Represents a cgroup that can limit CPU and memory resources.
/// Uses RAII pattern - automatically cleaned up when dropped.
pub struct Cgroup {
    path: PathBuf,
    cgroup: String,
}

impl Cgroup {
    /// Creates a new cgroup with the specified resource limits.
    ///
    /// # Arguments
    /// * `memory` - Optional memory limit (e.g., "100M", "1G")
    /// * `cpu` - Optional CPU quota as decimal (e.g., "0.5" for 50% of one core)
    ///
    /// # Returns
    /// A new Cgroup instance that will be automatically cleaned up on drop
    pub fn new(cpu: &Option<String>, memory: &Option<String>) -> Result<Self> {
        let cgroup_path = Path::new(CGROUP_PATH).join("toy_container");
        println!("Setting up cgroup {:?}", cgroup_path);

        let cgroup = Cgroup {
            path: cgroup_path,
            cgroup: String::from("leaf"),
        };

        // Ensure base cgroup directory exists and controllers are enabled
        cgroup
            .ensure_base_cgroup(memory.is_some(), cpu.is_some())
            .context("Failed to setup base cgroup")?;

        // Apply memory limit if specified
        if let Some(mem_limit) = memory {
            // Validate memory limit string before applying
            validate_memory_limit(mem_limit)?;

            cgroup
                .set_memory_limit(mem_limit)
                .with_context(|| format!("Failed to set memory limit to {}", mem_limit))?;
        }

        // Apply CPU limit if specified
        if let Some(cpu_quota) = cpu {
            cgroup
                .set_cpu_limit(cpu_quota)
                .with_context(|| format!("Failed to set CPU limit to {}", cpu_quota))?;
        }

        Ok(cgroup)
    }

    /// Adds a process to this cgroup.
    ///
    /// # Arguments
    /// * `pid` - Process ID to add to the cgroup
    pub fn add_process(&self, pid: i32) -> Result<()> {
        let procs_file = self.path.join(&self.cgroup).join("cgroup.procs");
        fs::write(&procs_file, pid.to_string())
            .with_context(|| format!("Failed to add process {} to cgroup", pid))?;
        Ok(())
    }

    /// Sets the memory limit for a cgroup.
    ///
    /// # Arguments
    /// * `path` - Path to the cgroup directory
    /// * `limit` - Memory limit string (e.g., "100M", "1G")
    pub fn set_memory_limit(&self, limit: &str) -> Result<()> {
        let memory_max = self.path.join(&self.cgroup).join("memory.max");
        fs::write(&memory_max, limit)
            .with_context(|| format!("Failed to write to {:?}", memory_max))?;
        Ok(())
    }

    /// Sets the CPU limit for a cgroup.
    ///
    /// # Arguments
    /// * `path` - Path to the cgroup directory
    /// * `quota` - CPU quota as a decimal string (e.g., "0.5" for 50%)
    pub fn set_cpu_limit(&self, quota: &str) -> Result<()> {
        let cpu_quota_str = parse_cpu_quota(quota)
            .with_context(|| format!("Failed to parse CPU quota '{}'", quota))?;

        let cpu_max = self.path.join(&self.cgroup).join("cpu.max");
        fs::write(&cpu_max, cpu_quota_str)
            .with_context(|| format!("Failed to write to {:?}", cpu_max))?;
        Ok(())
    }

    /// Ensures the base cgroup directory exists and controllers are enabled.
    fn ensure_base_cgroup(&self, need_memory: bool, need_cpu: bool) -> Result<()> {
        // Create cgroup directory if it doesn't exist
        let cgroup_dir = self.path.join(&self.cgroup);
        if !cgroup_dir.exists() {
            fs::create_dir_all(cgroup_dir)
                .with_context(|| format!("Failed to create base directory at {:?}", self.path))?;
        }

        // Build controller string
        let mut controllers = Vec::new();
        if need_memory {
            controllers.push("+memory");
        }
        if need_cpu {
            controllers.push("+cpu");
        }

        if !controllers.is_empty() {
            let controller_str = controllers.join(" ");

            // Enable controllers in the root cgroup's subtree_control
            // This allows us to use them in our cgroup
            let root_subtree_control = Path::new(CGROUP_PATH).join("cgroup.subtree_control");
            let _ = fs::write(root_subtree_control, &controller_str);

            // Also enable controllers in the silo cgroup's subtree_control
            // This allows child cgroups to use them
            let base_subtree_control = self.path.join("cgroup.subtree_control");
            let _ = fs::write(base_subtree_control, &controller_str);
        }

        Ok(())
    }
}

impl Drop for Cgroup {
    fn drop(&mut self) {
        // remove leaf cgroup
        let leaf_cgroup = self.path.join(self.cgroup.as_str());
        let _ = fs::remove_dir(&leaf_cgroup);
        // remove cgroup
        let _ = fs::remove_dir(&self.path);
    }
}

/// Parses a CPU quota decimal (e.g., "0.5") into cgroup format.
///
/// # Arguments
/// * `cpu` - CPU quota as decimal string (e.g., "0.5" for 50% of one core)
///
/// # Returns
/// A string in the format "quota period" (e.g., "50000 100000")
fn parse_cpu_quota(cpu: &str) -> Result<String> {
    let quota_fraction: f64 = cpu
        .parse()
        .context("CPU quota must be a valid decimal number")?;

    if quota_fraction <= 0.0 {
        anyhow::bail!("CPU quota must be greater than 0");
    }

    // Standard period is 100ms (100000 microseconds)
    const PERIOD: f64 = 100000.0;
    let quota = (quota_fraction * PERIOD) as i64;

    Ok(format!("{} {}", quota, PERIOD))
}

/// Validates a memory limit string for cgroup v2 `memory.max`.
///
/// Supported formats:
/// - "max" (no limit)
/// - Decimal number of bytes (e.g., "1048576")
/// - Number with unit suffix: K, M, G (decimal: 10^3, 10^6, 10^9)
/// - Number with IEC unit suffix: Ki, Mi, Gi (binary: 2^10, 2^20, 2^30)
///
/// Examples of valid inputs: "max", "1024", "512K", "128Ki", "100M", "2Gi"
///
/// Returns Ok(()) if valid, Err otherwise.
fn validate_memory_limit(limit: &str) -> Result<()> {
    let re = regex::Regex::new(r"^(?i:(max|\d+|\d+)(?:k|m|g|ki|mi|gi))$")?;
    if !re.is_match(limit) {
        anyhow::bail!(
            "Unsupported memory limit '{}'. Use: max, bytes, or units K/M/G/Ki/Mi/Gi",
            limit
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cpu_quota() {
        assert_eq!(parse_cpu_quota("0.5").unwrap(), "50000 100000");
        assert_eq!(parse_cpu_quota("1.0").unwrap(), "100000 100000");
        assert_eq!(parse_cpu_quota("2.0").unwrap(), "200000 100000");
        assert_eq!(parse_cpu_quota("0.25").unwrap(), "25000 100000");
    }

    #[test]
    fn test_parse_cpu_quota_invalid() {
        assert!(parse_cpu_quota("invalid").is_err());
        assert!(parse_cpu_quota("0").is_err());
        assert!(parse_cpu_quota("-0.5").is_err());
    }

    #[test]
    fn test_validate_memory_limit_valid() {
        // max and pure bytes
        assert!(validate_memory_limit("max").is_ok());
        assert!(validate_memory_limit("0").is_err()); // zero not allowed
        assert!(validate_memory_limit("1024").is_ok());

        // decimal units
        assert!(validate_memory_limit("1K").is_ok());
        assert!(validate_memory_limit("10M").is_ok());
        assert!(validate_memory_limit("2G").is_ok());

        // IEC units
        assert!(validate_memory_limit("1Ki").is_ok());
        assert!(validate_memory_limit("10Mi").is_ok());
        assert!(validate_memory_limit("2Gi").is_ok());

        // Case-insensitive units
        assert!(validate_memory_limit("5m").is_ok());
        assert!(validate_memory_limit("3gi").is_ok());
    }

    #[test]
    fn test_validate_memory_limit_invalid() {
        // Empty and non-numeric start
        assert!(validate_memory_limit("").is_err());
        assert!(validate_memory_limit("K100").is_err());

        // Missing unit value
        assert!(validate_memory_limit("K").is_err());

        // Unsupported units
        assert!(validate_memory_limit("100KB").is_err());
        assert!(validate_memory_limit("1TB").is_err());
        assert!(validate_memory_limit("1TiB").is_err());

        // Non-digit numeric part
        assert!(validate_memory_limit("1.5G").is_err());
        assert!(validate_memory_limit("abc").is_err());

        // Zero with unit
        assert!(validate_memory_limit("0M").is_err());
    }
}
