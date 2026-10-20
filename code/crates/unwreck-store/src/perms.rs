//! Unix 0700/0600 for the store. Windows ACLs are left to the OS.
use std::fs;
use std::io;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

const DIR_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;

pub(crate) fn ensure_private_dir(p: &Path) -> io::Result<()> {
    fs::create_dir_all(p)?;
    chmod_dir(p)
}

pub(crate) fn chmod_dir(p: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::set_permissions(p, fs::Permissions::from_mode(DIR_MODE))?;
    }
    let _ = p;
    Ok(())
}

pub(crate) fn chmod_file(p: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::set_permissions(p, fs::Permissions::from_mode(FILE_MODE))?;
    }
    let _ = p;
    Ok(())
}

pub(crate) fn create_private(p: &Path) -> io::Result<fs::File> {
    let mut o = fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    o.mode(FILE_MODE);
    o.open(p)
}

pub(crate) fn create_private_new(p: &Path) -> io::Result<fs::File> {
    let mut o = fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    o.mode(FILE_MODE);
    o.open(p)
}

pub(crate) fn open_lock(p: &Path) -> io::Result<fs::File> {
    let mut o = fs::OpenOptions::new();
    o.create(true).truncate(false).write(true).read(true);
    #[cfg(unix)]
    o.mode(FILE_MODE);
    o.open(p)
}

/// World/group bits that should be off for store dirs (`0700`) and files (`0600`).
#[cfg(unix)]
fn too_open(mode: u32, dir: bool) -> bool {
    let allow = if dir { DIR_MODE } else { FILE_MODE };
    mode & 0o777 & !allow != 0
}

/// Human-readable warnings if the store is looser than 0700/0600. Empty on Windows.
pub fn permission_warnings(root: &Path) -> Vec<String> {
    #[cfg(not(unix))]
    {
        let _ = root;
        return Vec::new();
    }
    #[cfg(unix)]
    {
        let mut out = Vec::new();
        let check_dir = |p: &Path, out: &mut Vec<String>| {
            if let Ok(m) = fs::metadata(p)
                && too_open(m.permissions().mode(), true)
            {
                out.push(format!(
                    "{} is mode {:04o} (want 0700)",
                    p.display(),
                    m.permissions().mode() & 0o777
                ));
            }
        };
        let check_file = |p: &Path, out: &mut Vec<String>| {
            if p.is_file()
                && let Ok(m) = fs::metadata(p)
                && too_open(m.permissions().mode(), false)
            {
                out.push(format!(
                    "{} is mode {:04o} (want 0600)",
                    p.display(),
                    m.permissions().mode() & 0o777
                ));
            }
        };
        check_dir(root, &mut out);
        for d in ["objects", "manifests", "tmp", "packs"] {
            check_dir(&root.join(d), &mut out);
        }
        for f in ["LOCK", "tip.json", "statcache.bin"] {
            check_file(&root.join(f), &mut out);
        }
        out
    }
}
