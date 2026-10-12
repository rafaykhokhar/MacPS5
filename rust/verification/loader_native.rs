use native_ps5::loader::Image;
use std::ffi::c_void;
unsafe extern "C" {
    fn native_ps5_loader_oracle_new(data: *const u8, size: u64) -> *mut c_void;
    fn native_ps5_loader_oracle_delete(image: *mut c_void);
    fn native_ps5_loader_oracle_bias(image: *mut c_void) -> u64;
    fn native_ps5_loader_oracle_relocate(image: *mut c_void) -> u64;
    fn native_ps5_loader_oracle_read(
        image: *mut c_void,
        address: u64,
        out: *mut u8,
        size: u64,
    ) -> u64;
    fn native_ps5_loader_oracle_applied(image: *mut c_void) -> u64;
    fn native_ps5_loader_oracle_pending(image: *mut c_void) -> u64;
}
struct Oracle(*mut c_void);
impl Drop for Oracle {
    fn drop(&mut self) {
        unsafe {
            native_ps5_loader_oracle_delete(self.0);
        }
    }
}
fn main() {
    let bytes = std::fs::read(std::env::args().nth(1).expect("ELF path required")).unwrap();
    // SAFETY: constructor copies input; live oracle is exclusively owned until Drop;
    // all output buffers are valid for exactly the passed length.
    unsafe {
        let oracle = Oracle(native_ps5_loader_oracle_new(
            bytes.as_ptr(),
            bytes.len() as u64,
        ));
        assert!(!oracle.0.is_null());
        let bias = native_ps5_loader_oracle_bias(oracle.0);
        let mut image = Image::load(&bytes, bias, 256 * 1024 * 1024).unwrap();
        image
            .relocate(|name| {
                if name == "sceKernelDlsym" {
                    Some(0x1234567800)
                } else {
                    None
                }
            })
            .unwrap();
        let exports = image.exported_symbols().unwrap();
        assert_eq!(exports.len(), 67);
        assert_eq!(exports.iter().filter(|e| e.function).count(), 65);
        let mmap = exports.iter().find(|e| e.name == "mmap").unwrap();
        assert_eq!((mmap.address, mmap.size), (bias + 0x16ff60, 228));
        println!(
            "PASS: real homebrew dynamic metadata yields 67 defined visible exports, including 65 functions"
        );
        assert_eq!(native_ps5_loader_oracle_relocate(oracle.0), 0);
        assert_eq!(
            image.applied as u64,
            native_ps5_loader_oracle_applied(oracle.0)
        );
        assert_eq!(
            image.unresolved.len() as u64,
            native_ps5_loader_oracle_pending(oracle.0)
        );
        let mut total = 0;
        for segment in &image.segments {
            let mut expected = vec![0; segment.bytes.len()];
            assert_eq!(
                native_ps5_loader_oracle_read(
                    oracle.0,
                    segment.address - bias,
                    expected.as_mut_ptr(),
                    expected.len() as u64
                ),
                0
            );
            assert_eq!(
                segment.bytes, expected,
                "guest segment {:x}",
                segment.address
            );
            total += expected.len();
        }
        println!(
            "PASS: Rust ELF loader matches C++ oracle across {total} segment bytes, {} applied relocations and {} unresolved imports",
            image.applied,
            image.unresolved.len()
        );
    }
}
