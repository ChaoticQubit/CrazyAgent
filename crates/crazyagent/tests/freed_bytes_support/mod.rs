use std::alloc::{GlobalAlloc, Layout, System};

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("freed_bytes_support reads freed blocks with x86_64 or aarch64 loads only");

/// A copy of a freed block's bytes in a scratch block of the system allocator, filled by
/// `asm!` loads, whose outputs are initialized even where the freed block held uninitialized
/// bytes, so a scan of the copy reads defined bytes only. The scratch is wiped and freed on
/// drop.
pub struct Copied {
    scratch: *mut u8,
    layout: Layout,
    len: usize,
}

impl Copied {
    /// Copies `len` bytes of `block` into a new scratch block, word by word, then the tail
    /// byte by byte. Inputs: the block and its size. Output: the copy; a failed scratch
    /// allocation panics. Safety: `block` must be valid for reads of `len` bytes, as a block
    /// handed to `GlobalAlloc::dealloc` is for its layout's size.
    pub unsafe fn new(block: *const u8, len: usize) -> Self {
        let layout = Layout::from_size_align(len.max(1), 8).unwrap();
        let scratch = unsafe { System.alloc(layout) };
        assert!(!scratch.is_null(), "the scratch block for a freed copy");
        let mut at = 0;
        while at + 8 <= len {
            let word = unsafe { load_word(block.add(at)) };
            unsafe { scratch.add(at).cast::<u64>().write_unaligned(word) };
            at += 8;
        }
        while at < len {
            let byte = unsafe { load_byte(block.add(at)) };
            unsafe { scratch.add(at).write(byte) };
            at += 1;
        }
        Self {
            scratch,
            layout,
            len,
        }
    }

    /// The copied bytes, every one written by [`Copied::new`]. Output: the bytes.
    pub fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.scratch, self.len) }
    }
}

impl Drop for Copied {
    /// Wipes the scratch with volatile zeros, so a later block that reuses its memory holds
    /// no copy of a watched secret, then frees it to the system allocator.
    fn drop(&mut self) {
        for at in 0..self.len {
            unsafe { self.scratch.add(at).write_volatile(0) };
        }
        unsafe { System.dealloc(self.scratch, self.layout) }
    }
}

/// Loads the 8 bytes at `at` with one `ldr`, unaligned loads being allowed on normal
/// memory. Input: the address. Output: the word. Safety: `at` must be valid for reads of 8
/// bytes.
#[cfg(target_arch = "aarch64")]
unsafe fn load_word(at: *const u8) -> u64 {
    let word: u64;
    unsafe {
        std::arch::asm!("ldr {word}, [{at}]", at = in(reg) at, word = out(reg) word,
            options(nostack, readonly, preserves_flags))
    };
    word
}

/// Loads the byte at `at` with one `ldrb`. Input: the address. Output: the byte. Safety:
/// `at` must be valid for a read of 1 byte.
#[cfg(target_arch = "aarch64")]
unsafe fn load_byte(at: *const u8) -> u8 {
    let byte: u32;
    unsafe {
        std::arch::asm!("ldrb {byte:w}, [{at}]", at = in(reg) at, byte = out(reg) byte,
            options(nostack, readonly, preserves_flags))
    };
    byte as u8
}

/// Loads the 8 bytes at `at` with one `mov`. Input: the address. Output: the word. Safety:
/// `at` must be valid for reads of 8 bytes.
#[cfg(target_arch = "x86_64")]
unsafe fn load_word(at: *const u8) -> u64 {
    let word: u64;
    unsafe {
        std::arch::asm!("mov {word}, qword ptr [{at}]", at = in(reg) at, word = out(reg) word,
            options(nostack, readonly, preserves_flags))
    };
    word
}

/// Loads the byte at `at` with one `movzx`. Input: the address. Output: the byte. Safety:
/// `at` must be valid for a read of 1 byte.
#[cfg(target_arch = "x86_64")]
unsafe fn load_byte(at: *const u8) -> u8 {
    let byte: u32;
    unsafe {
        std::arch::asm!("movzx {byte:e}, byte ptr [{at}]", at = in(reg) at, byte = out(reg) byte,
            options(nostack, readonly, preserves_flags))
    };
    byte as u8
}

/// A copy holds every byte of a block, the 8-byte words and an odd tail alike, so a scan of
/// the copy finds any secret the block held.
#[test]
fn a_copy_reads_every_byte_of_a_block() {
    let layout = Layout::from_size_align(77, 1).unwrap();
    let block = unsafe { System.alloc(layout) };
    assert!(!block.is_null());
    for at in 0..77 {
        unsafe {
            block
                .add(at)
                .write((at as u8).wrapping_mul(7).wrapping_add(1))
        };
    }
    let copied = unsafe { Copied::new(block, 77) };
    let expected: Vec<u8> = (0..77u8)
        .map(|at| at.wrapping_mul(7).wrapping_add(1))
        .collect();
    assert_eq!(copied.bytes(), expected.as_slice());
    drop(copied);
    unsafe { System.dealloc(block, layout) };
}
