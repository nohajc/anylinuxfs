use std::{fmt::Display, process::Command};

use crate::devinfo::DevInfo;

pub struct Entry(String, String, Vec<String>);

impl Entry {
    pub fn new(disk: &str) -> Self {
        Entry(disk.to_owned(), String::default(), Vec::new())
    }

    pub fn disk(&self) -> &str {
        self.0.as_str()
    }

    pub fn header(&self) -> &str {
        self.1.as_str()
    }

    pub fn header_mut(&mut self) -> &mut String {
        &mut self.1
    }

    pub fn partitions(&self) -> &[String] {
        &self.2
    }

    pub fn partitions_mut(&mut self) -> &mut Vec<String> {
        &mut self.2
    }
}

pub struct List(Vec<Entry>);

impl Display for List {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for entry in &self.0 {
            if entry.partitions().is_empty() {
                continue;
            }
            writeln!(f, "{}", entry.disk())?;
            if !entry.header().is_empty() {
                writeln!(f, "{}", entry.header())?;
            }
            for partition in entry.partitions() {
                writeln!(f, "{}", partition)?;
            }
        }
        Ok(())
    }
}

fn trunc_with_ellipsis(s: &str, max_len: usize) -> String {
    if s.len() > max_len {
        format!("{}...", &s[..max_len - 3])
    } else {
        s.to_string()
    }
}

pub fn list_linux_partitions() -> anyhow::Result<List> {
    let mut disk_entries = Vec::new();

    let output = Command::new("diskutil")
        .arg("list")
        .output()
        .expect("Failed to execute diskutil");

    if !output.status.success() {
        return Err(anyhow::anyhow!("diskutil command failed"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut current_entry = None;

    for line in stdout.lines() {
        if line.starts_with("/dev/disk") {
            disk_entries.push(Entry::new(line));
            let last_idx = disk_entries.len() - 1;
            current_entry = disk_entries.get_mut(last_idx)
        } else if line.trim_start().starts_with("#:") {
            current_entry.as_mut().map(|entry| {
                entry.header_mut().push_str(line);
            });
        } else {
            let linux_fs = "Linux Filesystem";
            if line.contains(linux_fs) {
                let dev_info = line
                    .split_whitespace()
                    .last()
                    .map(|part| DevInfo::new(&format!("/dev/{part}")).ok())
                    .flatten();
                let line = match dev_info {
                    Some(dev_info) => {
                        // let mut replace_patterns = Vec::new();
                        let mut line = line.to_owned();
                        let fs_type = dev_info.fs_type().unwrap_or(linux_fs);
                        let label = trunc_with_ellipsis(
                            dev_info.label().unwrap_or("                       "),
                            23,
                        );
                        line = line.replace(
                            &format!("{}                        ", linux_fs),
                            &format!("{:>16} {:<23}", fs_type, label),
                        );

                        line
                    }
                    None => line.to_owned(),
                };
                current_entry.as_mut().map(|entry| {
                    entry.partitions_mut().push(line);
                });
            }
        }
    }
    Ok(List(disk_entries))
}
