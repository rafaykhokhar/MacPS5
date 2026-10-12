//! Standalone macOS ARM64 hardware verification of the Rust RDNA translator.
//! No third-party Objective-C binding or retained C++ translator is used.
use native_ps5::rdna_metal::{
    FloatPolicy, RenderStage, translate_float_block, translate_integer_block,
    translate_render_block,
};
include!("metal_ffi.rs");
fn v1(op: u32, dst: u32, src: u32) -> u32 {
    0x7e000000 | dst << 17 | op << 9 | src
}
fn v2(op: u32, dst: u32, a: u32, b: u32) -> u32 {
    op << 25 | dst << 17 | b << 9 | a
}
fn flush(bits: u32) -> u32 {
    if bits & 0x7f800000 == 0 {
        bits & 0x80000000
    } else {
        bits
    }
}
fn fp(a: u32, b: u32, multiply: bool) -> u32 {
    let a = f32::from_bits(flush(a));
    let b = f32::from_bits(flush(b));
    let result = if multiply { a * b } else { a + b };
    if result.is_nan() {
        0x7fc00000
    } else {
        flush(result.to_bits())
    }
}
fn main() {
    assert!(
        cfg!(all(target_os = "macos", target_arch = "aarch64")),
        "Requires Apple Silicon macOS"
    );
    let mut words = vec![
        v2(0x25, 3, 256, 1),
        v2(0x26, 4, 256, 1),
        v2(0x27, 5, 256, 1),
        v2(0x1b, 6, 256, 1),
        v2(0x1c, 7, 256, 1),
        v2(0x1d, 8, 256, 1),
        v2(0x1e, 9, 256, 1),
        v2(0x1a, 10, 258, 0),
        v2(0x16, 11, 258, 0),
        v1(0x37, 12, 256),
        v1(1, 13, 255),
        0xdeadbeef,
        v1(1, 14, 5),
        v1(1, 15, 193),
        v1(1, 16, 242),
        v2(0x25, 3, 259, 3),
        v2(0x1d, 17, 269, 14),
        v1(1, 255, 273),
    ];
    // Cover every supported scalar/inline source encoding on actual hardware.
    let source_codes: Vec<u32> = (0..=105).chain(128..=208).chain(240..=248).collect();
    for (index, code) in source_codes.iter().enumerate() {
        words.push(v1(1, 32 + index as u32, *code));
    }
    words.extend([
        v2(3, 228, 274, 19),
        v2(8, 229, 274, 19),
        v2(3, 230, 485, 20),
        v2(3, 231, 255, 18),
        0x40000000,
        v2(8, 232, 244, 18),
        0xbf810000,
    ]);
    assert!(translate_integer_block(&words).is_err());
    let source = translate_float_block(&words, FloatPolicy::NearestEvenFlushToZero).unwrap();
    const LANES: usize = 8193;
    const GUARDS: usize = 3;
    const REGISTERS: usize = 256;
    let mut random = 0x6d2b79f5_u32;
    let mut input: Vec<u32> = (0..(LANES + GUARDS) * REGISTERS)
        .map(|_| {
            random = random.wrapping_mul(1664525).wrapping_add(1013904223);
            random
        })
        .collect();
    let scalars: Vec<u32> = (0..106_u32).map(|i| i.wrapping_mul(0x1020304)).collect();
    let active: Vec<u32> = (0..LANES)
        .map(|i| u32::from(i < 1025 || i % 5 != 0))
        .collect();
    let boundary = [0, 1, 31, 32, 33, 0x7fffffff, 0x80000000, u32::MAX];
    let float_boundary = [
        0, 0x80000000, 1, 0x80000001, 0x007fffff, 0x807fffff, 0x00800000, 0x80800000, 0x00800001,
        0x80800001, 0x3f800000, 0xbf800000, 0x3f800001, 0x3f7ffffe, 0x33800000, 0xb3800000,
        0x40000000, 0xc0000000, 0x3f000000, 0xbf000000, 0x7f7fffff, 0xff7fffff, 0x7f800000,
        0xff800000, 0x7fc12345, 0xffc54321, 0x7f800001, 0xff800001, 0x4b000001, 0x3eaaaaab,
        0x00800002, 0x3f7fffff,
    ];
    for i in 0..LANES {
        if i < 64 {
            input[i * 256] = boundary[i % 8];
            input[i * 256 + 1] = boundary[i / 8];
        }
        input[i * 256 + 2] = if i < 64 { i as u32 } else { boundary[i % 8] };
        if i < 1024 {
            input[i * 256 + 18] = float_boundary[i % 32];
            input[i * 256 + 19] = float_boundary[(i / 32) % 32];
        }
        input[i * 256 + 20] = 0xbf800000;
    }
    input[1024 * 256 + 18] = 0x3f800001;
    input[1024 * 256 + 19] = 0x3f7ffffe;
    // SAFETY: Exact Objective-C ABI signatures, finite host buffers, synchronous
    // command completion before reading shared output, and an autorelease pool
    // lasting beyond every command/object use. No guest pointers cross this FFI.
    unsafe {
        let pool_class = objc_getClass(c"NSAutoreleasePool".as_ptr());
        let pool = Owned::checked(msg!(pool_class,"new" => Obj));
        let device = Owned::checked(MTLCreateSystemDefaultDevice());
        assert!(msg!(device.0,"hasUnifiedMemory" => bool));
        let compiled = library(device.0, &source);
        let function = Owned::checked(
            msg!(compiled.0,"newFunctionWithName:" => Obj, Obj => string("rdna_block")),
        );
        let mut error: Obj = std::ptr::null_mut();
        let pipeline = Owned::checked(
            msg!(device.0,"newComputePipelineStateWithFunction:error:" => Obj,
            Obj => function.0, *mut Obj => &mut error),
        );
        let initial_output = vec![0xa5a5a5a5_u32; input.len()];
        let buffers: Vec<Owned> = [&input[..], &scalars[..], &initial_output[..], &active[..]]
            .iter()
            .map(|data| {
                Owned::checked(msg!(device.0,"newBufferWithBytes:length:options:" => Obj,
                *const u32 => data.as_ptr(), usize => std::mem::size_of_val(*data), usize => 0))
            })
            .collect();
        let queue = Owned::checked(msg!(device.0,"newCommandQueue" => Obj));
        let command = msg!(queue.0,"commandBuffer" => Obj);
        assert!(!command.is_null());
        let encoder = msg!(command,"computeCommandEncoder" => Obj);
        assert!(!encoder.is_null());
        msg!(encoder,"setComputePipelineState:" => (), Obj => pipeline.0);
        for (buffer, index) in buffers.iter().zip([0_usize, 1, 2, 4]) {
            msg!(encoder,"setBuffer:offset:atIndex:" => (),Obj => buffer.0,usize => 0,usize => index);
        }
        let lanes = LANES as u32;
        msg!(encoder,"setBytes:length:atIndex:" => (),*const u32 => &lanes,usize => 4,usize => 3);
        let group = 64.min(msg!(pipeline.0,"maxTotalThreadsPerThreadgroup" => usize));
        msg!(encoder,"dispatchThreads:threadsPerThreadgroup:" => (),
            Size => Size{width:LANES+GUARDS,height:1,depth:1},Size => Size{width:group,height:1,depth:1});
        msg!(encoder,"endEncoding" => ());
        msg!(command,"commit" => ());
        msg!(command,"waitUntilCompleted" => ());
        assert_eq!(
            msg!(command,"status" => usize),
            4,
            "Metal command did not complete"
        );
        let pointer = msg!(buffers[2].0,"contents" => *const u32);
        assert!(!pointer.is_null());
        let output = std::slice::from_raw_parts(pointer, input.len());
        for i in 0..LANES {
            let mut expected = input[i * 256..(i + 1) * 256].to_vec();
            if active[i] != 0 {
                let (a, b, shift) = (expected[0], expected[1], expected[2] & 31);
                expected[3] = a.wrapping_add(b).wrapping_mul(2);
                expected[4] = a.wrapping_sub(b);
                expected[5] = b.wrapping_sub(a);
                expected[6] = a & b;
                expected[7] = a | b;
                expected[8] = a ^ b;
                expected[9] = !(a ^ b);
                expected[10] = a << shift;
                expected[11] = a >> shift;
                expected[12] = !a;
                expected[13] = 0xdeadbeef;
                expected[14] = scalars[5];
                expected[15] = u32::MAX;
                expected[16] = 0x3f800000;
                expected[17] = 0xdeadbeef ^ scalars[5];
                expected[255] = expected[17];
                expected[228] = fp(expected[18], expected[19], false);
                expected[229] = fp(expected[18], expected[19], true);
                expected[230] = fp(expected[229], expected[20], false);
                expected[231] = fp(0x40000000, expected[18], false);
                expected[232] = fp(0x40000000, expected[18], true);
                for (index, code) in source_codes.iter().enumerate() {
                    expected[32 + index] = match *code {
                        0..=105 => scalars[*code as usize],
                        128..=192 => code - 128,
                        193..=208 => (192_i32 - *code as i32) as u32,
                        240..=247 => [0.5_f32, -0.5, 1.0, -1.0, 2.0, -2.0, 4.0, -4.0]
                            [(*code - 240) as usize]
                            .to_bits(),
                        248 => std::f32::consts::FRAC_1_PI.mul_add(0.5, 0.0).to_bits(),
                        _ => unreachable!(),
                    };
                }
            }
            for (register, expected) in expected.iter().enumerate() {
                assert_eq!(
                    output[i * 256 + register],
                    *expected,
                    "GPU mismatch at lane {i}, register {register}"
                );
            }
        }
        assert!(
            output[LANES * 256..].iter().all(|word| *word == 0xa5a5a5a5),
            "GPU guard write"
        );
        for (stage, export, name) in [
            (RenderStage::Vertex, 0xf80008cf, "rdna_vertex"),
            (RenderStage::Fragment, 0xf800180f, "rdna_fragment"),
        ] {
            let source = translate_render_block(&[export, 0x03020100, 0xbf810000], stage).unwrap();
            let library = library(device.0, &source);
            let _function =
                Owned::checked(msg!(library.0,"newFunctionWithName:" => Obj,Obj => string(name)));
        }
        let name = msg!(device.0,"name" => Obj);
        let name = CStr::from_ptr(msg!(name,"UTF8String" => *const c_char)).to_string_lossy();
        println!(
            "PASS: Rust RDNA decoder -> generated MSL -> {name}: {} register comparisons, inactive lanes and {} guard values preserved; vertex/fragment MSL compiled",
            LANES * REGISTERS,
            GUARDS * REGISTERS
        );
        assert_eq!(
            output[1024 * 256 + 230],
            0,
            "Multiply-add was incorrectly fused"
        );
        println!(
            "Fixed-policy V_ADD_F32/V_MUL_F32: 1024 boundary pairs, seeded random pairs and unfused-chain case; nearest-even, signed denormal flush, canonical NaNs. Experimental policy, not full console FP parity or gameplay"
        );
        drop(queue);
        drop(buffers);
        drop(pipeline);
        drop(function);
        drop(compiled);
        drop(device);
        drop(pool);
    }
}
