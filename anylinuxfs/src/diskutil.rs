use std::{fmt::Display, process::Command};

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
            if line.contains("Linux Filesystem") {
                current_entry.as_mut().map(|entry| {
                    entry.partitions_mut().push(line.to_owned());
                });
            }
        }
    }
    Ok(List(disk_entries))
}
