use super::sandbox::row;
use crate::perf_kit::{SAMPLES, time};
use crate::sandbox::elf::tests::executable;
use crate::sandbox::elf::{Lookup, glibc_program, lookup};
use std::fs;
use std::hint::black_box;

const GLIBC_LOADER: &str = "/lib64/ld-linux-x86-64.so.2";
const MUSL_LOADER: &str = "/lib/ld-musl-x86_64.so.1";

/// Times the ELF reader on two small 64-bit executables in a temp folder, one loaded by
/// glibc and one by musl, read in turn: how each looks names up, and whether it is a glibc
/// program. Prints one PERF line per call and asserts no time.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn elf_reader() {
    let temp = tempfile::tempdir().unwrap();
    let programs = [
        temp.path().join("glibc-program"),
        temp.path().join("musl-program"),
    ];
    fs::write(&programs[0], executable(true, false, Some(GLIBC_LOADER))).unwrap();
    fs::write(&programs[1], executable(true, false, Some(MUSL_LOADER))).unwrap();
    assert_eq!(lookup(&programs[0]), Lookup::OverTcp);
    assert_eq!(lookup(&programs[1]), Lookup::NotOverTcp);
    let mut at = 0usize;
    time(&row("elf.lookup", 10), SAMPLES, || {
        at += 1;
        black_box(lookup(black_box(&programs[at % 2])));
    });
    time(&row("elf.glibc_program", 10), SAMPLES, || {
        at += 1;
        black_box(glibc_program(black_box(&programs[at % 2])));
    });
}
