//! Stateful guarded guest DCB -> reusable Rust Metal queue verification.
use native_ps5::{
    graphics::Queue,
    guest_memory::Memory,
    metal_render::{Frame, Inputs},
    pm4::State,
};
fn put(bytes: &mut [u8], offset: usize, value: u64, size: usize) {
    bytes[offset..offset + size].copy_from_slice(&value.to_le_bytes()[..size]);
}
fn encode(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|v| v.to_le_bytes()).collect()
}
fn submit(
    queue: &mut Queue,
    words: &[u32],
    vertices: &[[u32; 256]],
    width: u32,
) -> Result<Frame, String> {
    let mut command = encode(words);
    let mut descriptor = [0_u8; 16];
    put(&mut descriptor, 0, 0x40000, 8);
    put(&mut descriptor, 8, words.len() as u64, 8);
    let mut memory = Memory::default();
    memory.add(0x40000, &mut command, false, false)?;
    memory.add(0x50000, &mut descriptor, false, false)?;
    queue.submit(
        &memory,
        0x50000,
        Inputs {
            vertices,
            fragment: &[0; 256],
            scalars: &[0; 106],
        },
        width,
        256,
    )
}
fn main() {
    assert!(cfg!(all(target_os = "macos", target_arch = "aarch64")));
    let vertex = [
        0x7e080303, 0x7e0a0302, 0x7e0c0301, 0x7e0e0300, 0xf80008cf, 0x04050607, 0x7e0e0280,
        0xbf810000,
    ];
    let fragment = [
        0x7e0002f2, 0x7e0202ff, 0x3dcccccd, 0x7e0402ff, 0x3e4ccccd, 0x7e0602ff, 0x3f4ccccd,
        0xf800180f, 0x00010203, 0x7e060280, 0xbf810000,
    ];
    let mut shadercode = vec![0_u8; 512];
    shadercode[..vertex.len() * 4].copy_from_slice(&encode(&vertex));
    shadercode[256..256 + fragment.len() * 4].copy_from_slice(&encode(&fragment));
    let mut headers = [0_u8; 192];
    for (index, base, size, stage) in [
        (0, 0x10000, vertex.len() * 4, 2),
        (1, 0x10100, fragment.len() * 4, 1),
    ] {
        let header = &mut headers[index * 96..(index + 1) * 96];
        for (offset, value, width) in [
            (0, 0x34333231, 4),
            (4, 0x18, 4),
            (16, base, 8),
            (64, 96, 4),
            (68, size as u64, 4),
            (90, stage, 1),
        ] {
            put(header, offset, value, width);
        }
    }
    let mut queue = Queue::default();
    {
        let mut memory = Memory::default();
        memory.add(0x10000, &mut shadercode, true, false).unwrap();
        memory.add(0x30000, &mut headers, true, false).unwrap();
        queue.register_shader(&memory, 0x30000).unwrap();
        queue.register_shader(&memory, 0x30060).unwrap();
        // Failed replacement cannot erase an already registered valid shader.
        memory.write(0x10000, &[0xff; 4]).unwrap();
        assert!(queue.register_shader(&memory, 0x30000).is_err());
        memory.write(0x30000, &[0; 4]).unwrap();
        assert!(queue.register_shader(&memory, 0x30000).is_err());
        memory.write(0x10100, &[0xff; 4]).unwrap();
    }
    drop(shadercode);
    headers.fill(0xff);
    let mut vertices = [[0_u32; 256]; 4];
    for (i, position) in [
        [10.0_f32, 10.0, 0.0, 1.0],
        [-0.8, -0.8, 0.0, 1.0],
        [0.8, -0.8, 0.0, 1.0],
        [0.0, 0.8, 0.0, 1.0],
    ]
    .iter()
    .enumerate()
    {
        for (r, value) in position.iter().enumerate() {
            vertices[i][r] = value.to_bits();
        }
    }
    let initial = [
        0xc0017900, 0x242, 4, 0xc0017900, 0x24a, 1, 0xc0002f00, 1, 0xc0027600, 0xc8, 0x100, 0,
        0xc0027600, 8, 0x101, 0, 0xc0012d00, 3, 2,
    ];
    let first = submit(&mut queue, &initial, &vertices, 256).unwrap();
    assert_ne!(queue.state(), &State::default());
    let saved = queue.state().clone();
    let saved_pixels = first.pixels.clone();
    // Invalid new vertex state must not leak into a subsequent inherited draw.
    assert!(
        submit(
            &mut queue,
            &[0xc0017900, 0x24a, 4, 0xc0012d00, 3, 2],
            &vertices,
            256
        )
        .is_err()
    );
    assert_eq!(queue.state(), &saved);
    // A valid PM4 state change followed by renderer rejection also rolls back.
    assert!(
        submit(
            &mut queue,
            &[0xc0017900, 0x24a, 0, 0xc0012d00, 3, 2],
            &vertices,
            0
        )
        .is_err()
    );
    assert_eq!(queue.state(), &saved);
    // An invalid packet after valid packets cannot partially advance state.
    assert!(
        submit(
            &mut queue,
            &[0xc0017900, 0x24a, 0, 0xffffffff],
            &vertices,
            256
        )
        .is_err()
    );
    assert_eq!(queue.state(), &saved);
    let second = submit(&mut queue, &[0xc0012d00, 3, 2], &vertices, 256).unwrap();
    assert_eq!(queue.state(), &saved);
    assert_eq!(second.pixels, saved_pixels);
    assert_eq!(first.pixels, saved_pixels);
    assert_eq!(
        (second.width, second.height, second.pixels.len()),
        (256, 256, 256 * 256 * 4)
    );
    let mut checked = 0;
    for y in 0..256 {
        for x in 0..256 {
            let nx = 2.0 * (x as f64 + 0.5) / 256.0 - 1.0;
            let ny = 1.0 - 2.0 * (y as f64 + 0.5) / 256.0;
            let a = (0.8 - ny) / 1.6;
            let b = (nx + 0.8 * a) / 1.6;
            let minimum = b.min(a - b).min(1.0 - a);
            let pixel = &second.pixels[(y * 256 + x) * 4..(y * 256 + x + 1) * 4];
            if minimum > 0.02 {
                assert!(
                    (203..=205).contains(&pixel[0])
                        && (50..=52).contains(&pixel[1])
                        && (25..=27).contains(&pixel[2])
                        && pixel[3] == 255
                );
            } else if minimum < -0.02 {
                assert_eq!(pixel, [0, 0, 0, 255]);
            } else {
                continue;
            }
            checked += 1;
        }
    }
    assert!(checked > 60000);
    println!(
        "PASS: reusable Rust graphics queue completes two guest DCB submissions on {} / {}; inherited PM4 state, failure rollback, immutable shader snapshots and {checked} reference pixels verified",
        first.device_name, second.device_name
    );
    println!(
        "Explicit raw-register fixture ABI only; guest AGC service dispatch and gameplay remain unsupported"
    );
}
