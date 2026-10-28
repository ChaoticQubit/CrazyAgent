use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const HEADER_BYTES: usize = 64;
const PT_INTERP: u32 = 3;
const PROGRAM_HEADERS_MAX: usize = 512;
const INTERPRETER_BYTES_MAX: u64 = 4096;
const GLIBC_LOADERS: &[&str] = &["ld64.so.1", "ld64.so.2"];
const GLIBC_LOADER_PREFIX: &str = "ld-linux";

/// True when `program` is an ELF executable whose program loader (its `PT_INTERP`) is
/// glibc's: `ld-linux*.so.*`, or `ld64.so.1`/`ld64.so.2` on PowerPC and s390x. Only glibc's
/// resolver reads `RES_OPTIONS=use-vc` and looks names up over TCP. A musl loader, a static
/// binary with no loader, a script or anything unreadable is false, so the caller fails
/// closed. Input: the program path, followed through links. Output: the answer.
pub fn glibc_program(program: &Path) -> bool {
    interpreter(program)
        .ok()
        .flatten()
        .is_some_and(|loader| glibc_loader(&loader))
}

fn glibc_loader(loader: &Path) -> bool {
    loader.is_absolute()
        && loader
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.starts_with(GLIBC_LOADER_PREFIX) || GLIBC_LOADERS.contains(&name)
            })
}

/// Reads the loader path an ELF executable names. Input: the file. Output: the loader,
/// None for an ELF file without one, or an error for a file that is not a well formed ELF
/// executable or cannot be read.
fn interpreter(program: &Path) -> std::io::Result<Option<std::path::PathBuf>> {
    let mut file = File::open(program)?;
    let mut header = [0u8; HEADER_BYTES];
    file.read_exact(&mut header)?;
    if header[..4] != MAGIC || header[6] != 1 {
        return Err(malformed());
    }
    let wide = match header[4] {
        1 => false,
        2 => true,
        _ => return Err(malformed()),
    };
    let big = match header[5] {
        1 => false,
        2 => true,
        _ => return Err(malformed()),
    };
    let field = |offset: usize, width: usize| -> u64 {
        assert!(offset + width <= HEADER_BYTES);
        number(&header[offset..offset + width], big)
    };
    if !matches!(field(16, 2), 2 | 3) {
        return Err(malformed());
    }
    let (table, entry, count, entry_bytes) = if wide {
        (field(32, 8), field(54, 2), field(56, 2), 56)
    } else {
        (field(28, 4), field(42, 2), field(44, 2), 32)
    };
    let count = usize::try_from(count).map_err(|_| malformed())?;
    if entry != entry_bytes || count > PROGRAM_HEADERS_MAX {
        return Err(malformed());
    }
    let mut headers = vec![0u8; count * entry_bytes as usize];
    file.seek(SeekFrom::Start(table))?;
    file.read_exact(&mut headers)?;
    let mut loaders = headers
        .chunks_exact(entry_bytes as usize)
        .filter(|header| number(&header[..4], big) == u64::from(PT_INTERP))
        .map(|header| {
            if wide {
                (number(&header[8..16], big), number(&header[32..40], big))
            } else {
                (number(&header[4..8], big), number(&header[16..20], big))
            }
        });
    let Some((offset, size)) = loaders.next() else {
        return Ok(None);
    };
    if loaders.next().is_some() || !(2..=INTERPRETER_BYTES_MAX).contains(&size) {
        return Err(malformed());
    }
    let mut path = vec![0u8; size as usize];
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut path)?;
    let Some((0, text)) = path.split_last() else {
        return Err(malformed());
    };
    if text.contains(&0) {
        return Err(malformed());
    }
    let text = std::str::from_utf8(text).map_err(|_| malformed())?;
    Ok(Some(std::path::PathBuf::from(text)))
}

fn number(bytes: &[u8], big: bool) -> u64 {
    assert!(bytes.len() <= 8);
    let fold = |value: u64, byte: &u8| (value << 8) | u64::from(*byte);
    if big {
        bytes.iter().fold(0, fold)
    } else {
        bytes.iter().rev().fold(0, fold)
    }
}

fn malformed() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "not a well formed ELF executable",
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds the bytes of a minimal ELF executable: the file header, one PT_LOAD program
    /// header, a PT_INTERP header naming `loader` when given, and the loader path. Inputs:
    /// 64-bit or 32-bit, big or little endian, and the loader. Output: the file bytes.
    pub(crate) fn executable(wide: bool, big: bool, loader: Option<&str>) -> Vec<u8> {
        let put = |bytes: &mut Vec<u8>, value: u64, width: usize| {
            let mut field: Vec<u8> = (0..width).map(|at| (value >> (8 * at)) as u8).collect();
            if big {
                field.reverse();
            }
            bytes.extend(field);
        };
        let (header_bytes, entry_bytes) = if wide { (64, 56) } else { (52, 32) };
        let count = 1 + usize::from(loader.is_some());
        let path_at = (header_bytes + count * entry_bytes) as u64;
        let mut bytes = MAGIC.to_vec();
        bytes.extend([if wide { 2 } else { 1 }, if big { 2 } else { 1 }, 1, 0]);
        bytes.resize(16, 0);
        put(&mut bytes, 3, 2);
        put(&mut bytes, if wide { 62 } else { 3 }, 2);
        put(&mut bytes, 1, 4);
        let word = if wide { 8 } else { 4 };
        put(&mut bytes, 0, word);
        put(&mut bytes, header_bytes as u64, word);
        put(&mut bytes, 0, word);
        put(&mut bytes, 0, 4);
        put(&mut bytes, header_bytes as u64, 2);
        put(&mut bytes, entry_bytes as u64, 2);
        put(&mut bytes, count as u64, 2);
        put(&mut bytes, 64, 2);
        put(&mut bytes, 0, 2);
        put(&mut bytes, 0, 2);
        assert_eq!(bytes.len(), header_bytes);
        let mut entries = vec![(1u64, 0u64, 0u64)];
        if let Some(loader) = loader {
            entries.push((u64::from(PT_INTERP), path_at, loader.len() as u64 + 1));
        }
        for (kind, offset, size) in entries {
            put(&mut bytes, kind, 4);
            if wide {
                put(&mut bytes, 4, 4);
            }
            put(&mut bytes, offset, word);
            put(&mut bytes, 0, word);
            put(&mut bytes, 0, word);
            put(&mut bytes, size, word);
            put(&mut bytes, size, word);
            if !wide {
                put(&mut bytes, 4, 4);
            }
            put(&mut bytes, 1, word);
        }
        assert_eq!(bytes.len() as u64, path_at);
        if let Some(loader) = loader {
            bytes.extend(loader.as_bytes());
            bytes.push(0);
        }
        bytes
    }

    fn written(folder: &tempfile::TempDir, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = folder.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn only_a_program_glibc_loads_counts_as_glibc() {
        let folder = tempfile::tempdir().unwrap();
        let cases = [
            (
                "x86_64",
                true,
                false,
                Some("/lib64/ld-linux-x86-64.so.2"),
                true,
            ),
            (
                "aarch64",
                true,
                false,
                Some("/lib/ld-linux-aarch64.so.1"),
                true,
            ),
            ("i386", false, false, Some("/lib/ld-linux.so.2"), true),
            ("s390x", true, true, Some("/lib/ld64.so.1"), true),
            (
                "nix",
                true,
                false,
                Some("/nix/store/q2-glibc-2.39-52/lib/ld-linux-x86-64.so.2"),
                true,
            ),
            ("musl", true, false, Some("/lib/ld-musl-x86_64.so.1"), false),
            (
                "musl_arm",
                true,
                false,
                Some("/lib/ld-musl-aarch64.so.1"),
                false,
            ),
            ("static", true, false, None, false),
            ("relative", true, false, Some("ld-linux-x86-64.so.2"), false),
        ];
        for (name, wide, big, loader, glibc) in cases {
            let path = written(&folder, name, &executable(wide, big, loader));
            assert_eq!(glibc_program(&path), glibc, "{name}");
        }
    }

    #[test]
    fn a_script_a_broken_file_or_a_missing_one_is_not_glibc() {
        let folder = tempfile::tempdir().unwrap();
        let glibc = executable(true, false, Some("/lib64/ld-linux-x86-64.so.2"));
        assert!(glibc_program(&written(&folder, "whole", &glibc)));
        let script = written(
            &folder,
            "codex.js",
            b"#!/usr/bin/env node\nimport './x.js';\n",
        );
        assert!(!glibc_program(&script));
        let truncated = written(&folder, "truncated", &glibc[..glibc.len() - 4]);
        assert!(!glibc_program(&truncated));
        let mut unterminated = glibc.clone();
        *unterminated.last_mut().unwrap() = b'x';
        assert!(!glibc_program(&written(
            &folder,
            "unterminated",
            &unterminated
        )));
        let mut relocatable = glibc.clone();
        relocatable[16] = 1;
        assert!(!glibc_program(&written(
            &folder,
            "relocatable",
            &relocatable
        )));
        let mut wrong_entry = glibc.clone();
        wrong_entry[54] = 55;
        assert!(!glibc_program(&written(
            &folder,
            "wrong_entry",
            &wrong_entry
        )));
        assert!(!glibc_program(&folder.path().join("absent")));
        #[cfg(unix)]
        {
            let link = folder.path().join("link");
            std::os::unix::fs::symlink(folder.path().join("whole"), &link).unwrap();
            assert!(
                glibc_program(&link),
                "a link counts as the program it names"
            );
        }
        #[cfg(target_os = "macos")]
        assert!(!glibc_program(Path::new("/bin/ls")), "a Mach-O program");
    }
}
