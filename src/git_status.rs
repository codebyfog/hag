use std::{
    collections::HashMap,
    ffi::OsStr,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Status(u8, u8);

impl Status {
    fn conflict(self) -> bool {
        matches!(
            (self.0, self.1),
            (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D')
        )
    }

    pub fn badge(self) -> String {
        if self.conflict() {
            return "!".into();
        }
        if self.0 == b' ' {
            return char::from(self.1).to_string();
        }
        if self.1 == b' ' {
            return "S".into();
        }
        format!("S{}", char::from(self.1))
    }

    pub fn tooltip(self) -> String {
        if self.conflict() {
            return "Merge conflict".into();
        }
        fn describe(code: u8) -> &'static str {
            match code {
                b'M' => "Modified",
                b'A' => "Added",
                b'D' => "Deleted",
                b'R' => "Renamed",
                b'C' => "Copied",
                b'T' => "Type changed",
                _ => "Changed",
            }
        }
        match (self.0, self.1) {
            (b' ', work) => describe(work).into(),
            (index, b' ') => format!("Staged: {}", describe(index)),
            (index, work) => format!("Staged: {}\nUnstaged: {}", describe(index), describe(work)),
        }
    }
}

pub fn repository(project: &Path) -> Option<PathBuf> {
    let result = Command::new("git")
        .current_dir(project)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !result.status.success() {
        return None;
    }
    Some(Path::new(OsStr::from_bytes(result.stdout.strip_suffix(b"\n")?)).to_owned())
}

pub fn read(repo: &Path) -> HashMap<PathBuf, Status> {
    let mut files = HashMap::new();
    let Ok(output) = Command::new("git")
        .current_dir(repo)
        .args([
            "--no-optional-locks",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=no",
        ])
        .output()
    else {
        return files;
    };
    if !output.status.success() {
        return files;
    }
    let mut records = output.stdout.split(|byte| *byte == 0);
    while let Some(record) = records.next() {
        if record.len() < 4 {
            continue;
        }
        let status = Status(record[0], record[1]);
        let path = Path::new(OsStr::from_bytes(&record[3..]));
        files.insert(path.to_owned(), status);
        // In -z format a rename/copy destination is followed by its original name.
        if matches!(status.0, b'R' | b'C') || matches!(status.1, b'R' | b'C') {
            records.next();
        }
    }
    files
}
