//! Native executable-storage lifecycle checks; not a guest translator.
use native_ps5::jit_memory::{JitArena, JitCode};
unsafe fn call(code: &JitCode, offset: usize) -> u32 {
    let function: unsafe extern "C" fn() -> u32 =
        unsafe { std::mem::transmute(code.entry(offset).unwrap()) };
    unsafe { function() }
}
fn main() {
    // SAFETY: standalone unsigned verification process, no other JIT engine,
    // no signal handlers, and all code below is known ARM64 MOVZ W0/RET.
    unsafe {
        assert!(JitArena::new(0).is_err());
        assert!(JitArena::new(1).is_err());
        let arena = JitArena::new(16384).unwrap();
        assert!(JitArena::new(16384).is_err());
        assert!(arena.publish(&[]).is_err());
        assert_eq!(arena.used(), 0);
        let mut retained = Vec::new();
        for value in 0..2048u32 {
            let code = arena
                .publish(&[0x52800000 | (value << 5), 0xd65f03c0])
                .unwrap();
            assert_eq!(call(&code, 0), value);
            assert!(code.entry(1).is_err());
            assert!(code.entry(8).is_err());
            assert!(code.entry(usize::MAX).is_err());
            retained.push(code);
        }
        assert_eq!(arena.used(), arena.capacity());
        assert!(arena.publish(&[0xd65f03c0]).is_err());
        assert_eq!(arena.used(), 16384);
        drop(arena);
        assert!(
            JitArena::new(16384).is_err(),
            "Code handles pin the mapping"
        );
        for (value, code) in retained.iter().enumerate() {
            assert_eq!(call(code, 0), value as u32);
        }
        let last = retained.last().unwrap().clone();
        drop(retained);
        assert_eq!(call(&last, 0), 2047);
        assert!(JitArena::new(16384).is_err());
        drop(last);
        let next = JitArena::new(16384).unwrap();
        let code = next.publish(&[0x52800540, 0xd65f03c0]).unwrap();
        assert_eq!(call(&code, 0), 42);
    }
    println!(
        "PASS: 4098 native calls, immutable append, exact capacity, rejected ranges, pinned lifetime and remapping after final release"
    );
}
