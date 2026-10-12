//! PM4 -> bounded Rust render plan -> RDNA-generated MSL -> offscreen Metal.
//! Synthetic register-buffer ABI only; this does not implement AGC submission.
use native_ps5::{
    guest_memory::Memory, guest_shader, guest_submission, pm4, render_plan::Registry,
};

fn main() {
    assert!(
        cfg!(all(target_os = "macos", target_arch = "aarch64")),
        "Requires Apple Silicon macOS"
    );
    let vertex: [u32; 8] = [
        0x7e080303, 0x7e0a0302, 0x7e0c0301, 0x7e0e0300, 0xf80008cf, 0x04050607, 0x7e0e0280,
        0xbf810000,
    ];
    let fragment: [u32; 11] = [
        0x7e0002f2, 0x7e0202ff, 0x3dcccccd, 0x7e0402ff, 0x3e4ccccd, 0x7e0602ff, 0x3f4ccccd,
        0xf800180f, 0x00010203, 0x7e060280, 0xbf810000,
    ];
    let words: [u32; 20] = [
        0x80000000, 0xc0017900, 0x242, 4, 0xc0017900, 0x24a, 1, 0xc0002f00, 1, 0xc0027600, 0xc8,
        0x100, 0, 0xc0027600, 8, 0x200, 0, 0xc0012d00, 3, 2,
    ];
    let mut vertex_bytes: Vec<u8> = vertex.iter().flat_map(|v| v.to_le_bytes()).collect();
    let mut fragment_bytes: Vec<u8> = fragment.iter().flat_map(|v| v.to_le_bytes()).collect();
    let mut command_bytes: Vec<u8> = words.iter().flat_map(|v| v.to_le_bytes()).collect();
    let mut headers = [0_u8; 192];
    for (index, address, size, stage) in [
        (0, 0x10000, vertex.len() * 4, 2),
        (1, 0x20000, fragment.len() * 4, 1),
    ] {
        let header = &mut headers[index * 96..(index + 1) * 96];
        for (offset, value, width) in [
            (0, 0x34333231_u64, 4),
            (4, 0x18, 4),
            (16, address, 8),
            (64, 96, 4),
            (68, size as u64, 4),
            (90, stage, 1),
        ] {
            header[offset..offset + width].copy_from_slice(&value.to_le_bytes()[..width]);
        }
    }
    let mut descriptor = [0_u8; 16];
    descriptor[..8].copy_from_slice(&0x40000_u64.to_le_bytes());
    descriptor[8..].copy_from_slice(&(words.len() as u64).to_le_bytes());
    let mut memory = Memory::default();
    memory.add(0x10000, &mut vertex_bytes, true, false).unwrap();
    memory
        .add(0x20000, &mut fragment_bytes, true, false)
        .unwrap();
    memory.add(0x30000, &mut headers, true, false).unwrap();
    memory
        .add(0x40000, &mut command_bytes, true, false)
        .unwrap();
    memory.add(0x50000, &mut descriptor, true, false).unwrap();
    let mut registry = Registry::default();
    guest_shader::read(&memory, 0x30000)
        .unwrap()
        .register_simple(&mut registry)
        .unwrap();
    guest_shader::read(&memory, 0x30060)
        .unwrap()
        .register_simple(&mut registry)
        .unwrap();
    let submission = guest_submission::prepare(&memory, 0x50000, &pm4::State::default()).unwrap();
    let plans = registry.prepare(&submission.decoded, 4).unwrap();
    // Prepared draws retain the validated guest snapshots, not mutable pointers.
    for address in [0x10000, 0x20000, 0x30000, 0x40000, 0x50000] {
        memory.write(address, &[0xff; 4]).unwrap();
    }
    drop(memory);
    assert_eq!(plans.len(), 1);
    let mut vertices = vec![0_u32; 4 * 256];
    let positions = [
        [10.0_f32, 10.0, 0.0, 1.0],
        [-0.8, -0.8, 0.0, 1.0],
        [0.8, -0.8, 0.0, 1.0],
        [0.0, 0.8, 0.0, 1.0],
    ];
    for (i, position) in positions.iter().enumerate() {
        for (r, value) in position.iter().enumerate() {
            vertices[i * 256 + r] = value.to_bits();
        }
    }
    let fragment_input = [0_u32; 256];
    let scalars = [0_u32; 106];
    const WIDTH: usize = 256;

    let vertex_registers: Vec<[u32; 256]> = vertices
        .chunks_exact(256)
        .map(|chunk| chunk.try_into().unwrap())
        .collect();
    let frame = native_ps5::metal_render::render(
        &plans,
        native_ps5::metal_render::Inputs {
            vertices: &vertex_registers,
            fragment: &fragment_input,
            scalars: &scalars,
        },
        WIDTH as u32,
        WIDTH as u32,
    )
    .unwrap();
    assert_eq!((frame.width, frame.height), (WIDTH as u32, WIDTH as u32));
    assert!(
        native_ps5::metal_render::render(
            &plans,
            native_ps5::metal_render::Inputs {
                vertices: &vertex_registers[..3],
                fragment: &fragment_input,
                scalars: &scalars,
            },
            WIDTH as u32,
            WIDTH as u32
        )
        .is_err()
    );
    let clear = native_ps5::metal_render::render(
        &[],
        native_ps5::metal_render::Inputs {
            vertices: &vertex_registers,
            fragment: &fragment_input,
            scalars: &scalars,
        },
        32,
        17,
    )
    .unwrap();
    assert_eq!(
        (clear.width, clear.height, clear.pixels.len()),
        (32, 17, 32 * 17 * 4)
    );
    assert!(
        clear
            .pixels
            .chunks_exact(4)
            .all(|pixel| pixel == [0, 0, 0, 255])
    );
    let pixels = frame.pixels;
    let colored = pixels.chunks_exact(4).filter(|pixel| pixel[0] != 0).count();
    assert!(
        (20000..22000).contains(&colored),
        "Unexpected triangle coverage {colored}"
    );
    let mut checked = 0;
    for y in 0..WIDTH {
        for x in 0..WIDTH {
            // Independent half-plane reference excludes rasterizer edge rules.
            let nx = 2.0 * (x as f64 + 0.5) / WIDTH as f64 - 1.0;
            let ny = 1.0 - 2.0 * (y as f64 + 0.5) / WIDTH as f64;
            let a = (0.8 - ny) / 1.6;
            let b = (nx + 0.8 * a) / 1.6;
            let c = a - b;
            let minimum = b.min(c).min(1.0 - a);
            let pixel = &pixels[(y * WIDTH + x) * 4..(y * WIDTH + x + 1) * 4];
            if minimum > 0.02 {
                assert!(
                    (203..=205).contains(&pixel[0])
                        && (50..=52).contains(&pixel[1])
                        && (25..=27).contains(&pixel[2])
                        && pixel[3] == 255,
                    "Triangle pixel mismatch ({x},{y}): {pixel:?}"
                );
            } else if minimum < -0.02 {
                assert_eq!(pixel, &[0, 0, 0, 255], "Background mismatch ({x},{y})");
            } else {
                continue;
            }
            checked += 1;
        }
    }
    assert!(checked > 60000);
    if let Some(path) = std::env::args().nth(1) {
        let mut ppm = format!("P6\n{WIDTH} {WIDTH}\n255\n").into_bytes();
        for pixel in pixels.chunks_exact(4) {
            ppm.extend_from_slice(&pixel[..3]);
        }
        std::fs::write(path, ppm).unwrap();
    }
    println!(
        "PASS: guarded guest shader/DCB snapshots -> Rust PM4 -> validated render plan -> Rust RDNA -> Metal on {}: {colored} colored pixels, {checked} reference pixel comparisons",
        frame.device_name
    );
    println!(
        "Synthetic triangle only; console graphics submission and gameplay remain unsupported"
    );
}
