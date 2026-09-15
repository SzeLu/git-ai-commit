use crate::i18n::{t, t_args};
use anyhow::{Context, Result};
use std::process::Command;

pub fn is_git_repo() -> Result<bool> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output()?;
    Ok(output.status.success())
}

pub fn get_git_diff(all: bool) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.arg("diff");

    if !all {
        cmd.arg("--cached");
    }

    let output = cmd.output().context(t("git_diff_failed"))?;

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn get_git_status(all: bool) -> Result<String> {
    if all {
        let output = Command::new("git")
            .args(["status", "--short"])
            .output()
            .context(t("git_status_failed"))?;
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let output = Command::new("git")
            .args(["diff", "--cached", "--name-status"])
            .output()
            .context(t("git_status_failed"))?;
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

pub fn get_git_diff_stats(all: bool) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.arg("diff");

    if !all {
        cmd.arg("--cached");
    }

    cmd.arg("--stat");

    let output = cmd.output().context(t("git_diff_stats_failed"))?;

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub struct RepoInfo {
    pub branch: String,
    pub remote: String,
}

pub fn get_repo_info() -> Result<RepoInfo> {
    let branch = Command::new("git")
        .args(["branch", "--show-current"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    let remote = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    Ok(RepoInfo { branch, remote })
}

pub fn commit_with_message(message: &str) -> Result<()> {
    // 使用临时文件避免命令行参数长度限制
    let temp_file = tempfile::NamedTempFile::new()?;
    std::fs::write(temp_file.path(), message)?;

    let status = Command::new("git")
        .args(["commit", "-F", temp_file.path().to_str().unwrap()])
        .status()
        .context(t("git_commit_failed"))?;

    if status.success() {
        Ok(())
    } else {
        // 退出码是**数据**，不翻译；`-1` 表示被信号杀掉，没有退出码可报。
        anyhow::bail!(t_args(
            "git_commit_failed_code",
            &[("code", status.code().unwrap_or(-1).into())]
        ))
    }
}
