//! The public installer owns downloading, verification, and installation.

use anyhow::{Context, Result, bail};
use axel_i18n::t;

pub fn run() -> Result<()> {
    let status = std::process::Command::new("/bin/bash")
        .args(["-o", "pipefail", "-c", "curl -fsSL https://axel.md/install | sh"])
        .status()
        .context(t!("running the Axel install script"))?;
    if !status.success() { bail!("{}", t!("axel update failed: {status}", status = status)) }
    Ok(())
}
