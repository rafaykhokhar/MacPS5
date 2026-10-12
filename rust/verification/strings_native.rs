//! Rust test driver for the actual C++ dispatch -> Rust string -> guarded memory path.
use native_ps5::legacy_cpu::Cpu;
use std::ffi::c_void;
unsafe extern "C" {
    fn snprintf(out: *mut u8, size: usize, format: *const u8, ...) -> i32;
    fn native_ps5_legacy_memory_new() -> *mut c_void;
    fn native_ps5_legacy_memory_delete(p: *mut c_void);
    fn native_ps5_legacy_memory_add(
        p: *mut c_void,
        address: u64,
        bytes: *mut u8,
        size: u64,
        writable: u64,
        executable: u64,
    ) -> u64;
    fn native_ps5_legacy_services_new(p: *mut c_void) -> *mut c_void;
    fn native_ps5_legacy_services_delete(p: *mut c_void);
    fn native_ps5_legacy_resolve(p: *mut c_void, module: u64, name: *const u8, length: u64) -> u64;
    fn native_ps5_legacy_dispatch(p: *mut c_void, cpu: *mut Cpu) -> u64;
}
struct Runtime {
    memory: *mut c_void,
    services: *mut c_void,
}
impl Drop for Runtime {
    fn drop(&mut self) {
        // SAFETY: pointers were returned by the matching constructors, remain live,
        // and are destroyed once in dependency order while their backing lives.
        unsafe {
            native_ps5_legacy_services_delete(self.services);
            native_ps5_legacy_memory_delete(self.memory);
        }
    }
}
fn main() {
    let mut code = [0xccu8; 64];
    let mut data = [0xa5u8; 256];
    let mut ro = [0x77u8; 16];
    let mut stack = [0u8; 128];
    ro[15] = 0;
    // SAFETY: all backing arrays outlive the runtime, remain stationary and are
    // accessed by Rust only between synchronous foreign calls. CPU has the
    // checked C layout. None of the callbacks retain a Rust borrow.
    unsafe {
        let memory = native_ps5_legacy_memory_new();
        assert!(!memory.is_null());
        for (address, bytes, writable, executable) in [
            (0x1000, &mut code[..], 0, 1),
            (0x2000, &mut data[..], 1, 0),
            (0x3000, &mut ro[..], 0, 0),
            (0x4000, &mut stack[..], 1, 0),
        ] {
            assert_eq!(
                native_ps5_legacy_memory_add(
                    memory,
                    address,
                    bytes.as_mut_ptr(),
                    bytes.len() as u64,
                    writable,
                    executable
                ),
                0
            );
        }
        let runtime = Runtime {
            memory,
            services: native_ps5_legacy_services_new(memory),
        };
        assert!(!runtime.services.is_null());
        let invoke = |name: &str, a: u64, b: u64, n: u64| {
            let target =
                native_ps5_legacy_resolve(runtime.services, 2, name.as_ptr(), name.len() as u64);
            assert_ne!(target, 0);
            let mut cpu = Cpu {
                memory: runtime.memory as u64,
                stop: 1,
                target,
                next_rip: 0x1000,
                flags: 0x603,
                ..Cpu::default()
            };
            cpu.registers[0] = 0xdecafbad;
            cpu.registers[4] = 0x4080;
            cpu.registers[7] = a;
            cpu.registers[6] = b;
            cpu.registers[2] = n;
            let result = native_ps5_legacy_dispatch(runtime.services, &mut cpu);
            (result, cpu)
        };
        data[..5].copy_from_slice(b"PATH\0");
        let (ok, env) = invoke("getenv", 0x2000, 0, 0);
        assert_eq!((ok, env.registers[0], env.fault), (1, 0, 0));
        assert_eq!(env.registers[4], 0x4080);
        assert_eq!(env.flags, 0x603);
        let (ok, bad_env) = invoke("getenv", u64::MAX, 0, 0);
        assert_eq!((ok, bad_env.fault), (0, 1));
        assert_eq!(bad_env.registers[0], 0xdecafbad);
        println!(
            "PASS: Rust getenv dispatch uses the guest's empty environment and faults on invalid name memory"
        );
        let (ok, allocated) = invoke("calloc", 3, 7, 0);
        assert_eq!(ok, 1);
        let address = allocated.registers[0];
        assert_ne!(address, 0);
        assert_eq!(address % 16, 0);
        // Read all requested bytes through guarded guest strings, never host pointer casts.
        for offset in 0..21 {
            let (ok, result) = invoke("strlen", address + offset, 0, 0);
            assert_eq!((ok, result.registers[0]), (1, 0));
        }
        data[..6].copy_from_slice(b"heap!\0");
        assert_eq!(invoke("strcpy", address, 0x2000, 0).0, 1);
        assert_eq!(invoke("strlen", address, 0, 0).1.registers[0], 5);
        let (ok, bad_free) = invoke("free", address + 1, 0, 0);
        assert_eq!((ok, bad_free.fault), (0, 8));
        assert_eq!(invoke("strlen", address, 0, 0).1.registers[0], 5);
        assert_eq!(invoke("free", address, 0, 0).0, 1);
        let (ok, released) = invoke("strlen", address, 0, 0);
        assert_eq!((ok, released.fault), (0, 1));
        assert_eq!(invoke("free", address, 0, 0).1.fault, 8);
        assert_eq!(invoke("free", 0, 0, 0).0, 1);
        let (ok, overflow) = invoke("calloc", u64::MAX, 2, 0);
        assert_eq!((ok, overflow.registers[0], overflow.fault), (1, 0, 0));
        let (ok, allocated) = invoke("malloc", 21, u64::MAX, 0);
        assert_eq!((ok, allocated.fault), (1, 0));
        let address = allocated.registers[0];
        assert_ne!(address, 0);
        assert_eq!(address % 16, 0);
        assert_eq!(invoke("strcpy", address, 0x2000, 0).0, 1);
        assert_eq!(invoke("strlen", address, 0, 0).1.registers[0], 5);
        assert_eq!(invoke("free", address, 0, 0).0, 1);
        assert_eq!(invoke("strlen", address, 0, 0).1.fault, 1);
        let (ok, oversized) = invoke("malloc", u64::MAX, 0, 0);
        assert_eq!((ok, oversized.registers[0], oversized.fault), (1, 0, 0));
        // Deliberately leave an allocation live to exercise service-owned teardown.
        assert_ne!(invoke("calloc", 0, 100, 0).1.registers[0], 0);
        println!(
            "PASS: Rust calloc/free through guest dispatch: zeroing, alignment, writable mappings, overflow, invalid frees and revoked access"
        );
        data[..6].copy_from_slice(b"hello\0");
        data[16..23].copy_from_slice(b" world\0");
        let (ok, c) = invoke("strcpy", 0x2040, 0x2000, 0);
        assert_eq!(ok, 1);
        assert_eq!(c.registers[0], 0x2040);
        assert_eq!(c.registers[4], 0x4080);
        assert_eq!(c.rip, 0x1000);
        assert_eq!(c.flags, 0x603);
        assert_eq!(&data[64..70], b"hello\0");
        let (ok, c) = invoke("strcat", 0x2040, 0x2010, 0);
        assert_eq!(ok, 1);
        assert_eq!(c.registers[0], 0x2040);
        assert_eq!(&data[64..76], b"hello world\0");
        let (ok, c) = invoke("strncmp", 0x2000, 0x2040, 5);
        assert_eq!((ok, c.registers[0]), (1, 0));
        let (ok, c) = invoke("strncmp", 0x2000, 0x2040, 6);
        assert_eq!((ok, c.registers[0]), (1, (-32i32) as u32 as u64));
        let (ok, c) = invoke("strlen", 0x2040, 0, 0);
        assert_eq!((ok, c.registers[0]), (1, 11));
        let (ok, c) = invoke("strlen", 0x300f, 0, 0);
        assert_eq!((ok, c.registers[0]), (1, 0));
        let (ok, c) = invoke("strncmp", u64::MAX, u64::MAX, 0);
        assert_eq!((ok, c.registers[0]), (1, 0));
        for a in 0..=255 {
            for b in 0..=255 {
                data[0] = a;
                data[1] = 0;
                data[16] = b;
                data[17] = 0;
                let (ok, c) = invoke("strcmp", 0x2000, 0x2010, 0);
                assert_eq!(ok, 1);
                assert_eq!(c.registers[0], (i32::from(a) - i32::from(b)) as u32 as u64);
                assert_eq!(c.flags, 0x603);
            }
        }
        data[..4].copy_from_slice(b"abc\0");
        let before = data;
        for (name, a, b, n, fault) in [
            ("strcpy", 0x3000, 0x2000, 0, 2),
            ("strcpy", 0x20ff, 0x2000, 0, 2),
            ("strcpy", u64::MAX, 0x2000, 0, 2),
            ("strcpy", 0x2040, 0x20ff, 0, 1),
            ("strlen", u64::MAX, 0, 0, 1),
            ("strcat", 0x20ff, 0x2000, 0, 1),
        ] {
            let (ok, c) = invoke(name, a, b, n);
            assert_eq!(ok, 0);
            assert_eq!(c.fault, fault);
            assert_eq!(c.registers[0], 0xdecafbad);
            assert_eq!(c.flags, 0x603);
            assert_eq!(c.registers[4], 0x4078);
            assert_eq!(data, before);
        }
        data[..2].copy_from_slice(b"X\0");
        let (ok, c) = invoke("strcpy", 0x4078, 0x2000, 0);
        assert_eq!(ok, 0);
        assert_eq!(c.fault, 6);
        assert_eq!(stack[120], b'X');
        let export = native_ps5::strings::export(0);
        assert_eq!(&export.name[..7], b"strcpy\0");
        println!(
            "PASS: actual Rust string dispatch, 65,536 unsigned comparisons, copy/append/length, guest returns, read-only and crossing-range faults, and return-address aliasing"
        );
        let format_call = |arguments: [u64; 4], destination: u64, stack_top: u64| {
            let target = native_ps5_legacy_resolve(runtime.services, 2, b"sprintf".as_ptr(), 7);
            assert_ne!(target, 0);
            let mut cpu = Cpu {
                memory: runtime.memory as u64,
                stop: 1,
                target,
                next_rip: 0x1000,
                flags: 0x603,
                ..Cpu::default()
            };
            cpu.registers[0] = 0xdecafbad;
            cpu.registers[4] = stack_top;
            cpu.registers[7] = destination;
            cpu.registers[6] = 0x2000;
            for (index, value) in [2, 1, 8, 9].into_iter().zip(arguments) {
                cpu.registers[index] = value;
            }
            let result = native_ps5_legacy_dispatch(runtime.services, &mut cpu);
            (result, cpu)
        };
        let mut format_checks = 0;
        for (fmt, kind) in [
            ("%d", 0),
            ("%+08d", 0),
            ("% 12i", 0),
            ("%.0d", 0),
            ("%hhd", 0),
            ("%hd", 0),
            ("%012.7d", 0),
            ("%u", 1),
            ("%#x", 1),
            ("%#08X", 1),
            ("%#.0o", 1),
            ("%10.4u", 1),
            ("%lld", 2),
            ("%#llx", 3),
            ("%zu", 4),
        ] {
            let mut fmt = fmt.as_bytes().to_vec();
            fmt.push(0);
            for value in [0, 1, 15, 255, 0x7fffffff, 0x80000000, 1u64 << 63, u64::MAX] {
                let mut host = [0u8; 128];
                let length = match kind {
                    0 => snprintf(host.as_mut_ptr(), host.len(), fmt.as_ptr(), value as i32),
                    1 => snprintf(host.as_mut_ptr(), host.len(), fmt.as_ptr(), value as u32),
                    2 => snprintf(host.as_mut_ptr(), host.len(), fmt.as_ptr(), value as i64),
                    3 => snprintf(host.as_mut_ptr(), host.len(), fmt.as_ptr(), value),
                    _ => snprintf(host.as_mut_ptr(), host.len(), fmt.as_ptr(), value as usize),
                };
                assert!((0..128).contains(&length));
                data[..fmt.len()].copy_from_slice(&fmt);
                let (ok, c) = format_call([value, 0, 0, 0], 0x2080, 0x4040);
                assert_eq!(ok, 1);
                assert_eq!(c.registers[0], length as u64);
                assert_eq!(
                    &data[128..128 + length as usize + 1],
                    &host[..length as usize + 1]
                );
                assert_eq!(c.registers[4], 0x4040);
                assert_eq!(c.flags, 0x603);
                format_checks += 1;
            }
        }
        data[..7].copy_from_slice(b"%*.*u\0\0");
        let (ok, c) = format_call([(-8i64) as u64, 4, 42, 0], 0x2080, 0x4040);
        assert_eq!((ok, c.registers[0]), (1, 8));
        assert_eq!(&data[128..137], b"0042    \0");
        data[..18].copy_from_slice(b"%u %u %u %u %u %u\0");
        stack[64..72].copy_from_slice(&5u64.to_le_bytes());
        stack[72..80].copy_from_slice(&6u64.to_le_bytes());
        let (ok, c) = format_call([1, 2, 3, 4], 0x2080, 0x4040);
        assert_eq!((ok, c.registers[0]), (1, 11));
        assert_eq!(&data[128..140], b"1 2 3 4 5 6\0");
        for (fmt, args, destination, stack_top, fault) in [
            (b"%n\0".as_slice(), [0; 4], 0x2080, 0x4040, 8),
            (b"%f\0", [0; 4], 0x2080, 0x4040, 8),
            (b"%s\0", [u64::MAX, 0, 0, 0], 0x2080, 0x4040, 1),
            (b"abc\0", [0; 4], 0x3000, 0x4040, 2),
            (b"%200u\0", [0; 4], 0x2080, 0x4040, 2),
            (b"%u%u%u%u%u\0", [0; 4], 0x2080, 0x4080, 1),
        ] {
            data[..fmt.len()].copy_from_slice(fmt);
            let before = data;
            let (ok, c) = format_call(args, destination, stack_top);
            assert_eq!(ok, 0);
            assert_eq!(c.fault, fault);
            assert_eq!(c.registers[0], 0xdecafbad);
            assert_eq!(data, before);
        }
        println!(
            "PASS: Rust sprintf matches {format_checks} typed host-libc cases; guest register/stack arguments, star precision, output preservation and guarded errors"
        );
    }
}
