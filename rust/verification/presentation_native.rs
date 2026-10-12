//! Host presentation check using a fixture tiled with the actual ELF's table.
use native_ps5::{loader::Image, metal_present::Presenter, video_tile};
fn main() -> Result<(), String> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("Missing pinned homebrew ELF")?;
    let file = std::fs::read(path).map_err(|e| e.to_string())?;
    let image = Image::load(&file, 0, 64 * 1024 * 1024)?;
    let base = 0x534170u64;
    let segment = image
        .segments
        .iter()
        .find(|s| base >= s.address && base + 131072 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing pinned tile table")?;
    let offset = (base - segment.address) as usize;
    let table = &segment.bytes[offset..offset + 131072];
    let (width, height) = (640usize, 360usize);
    std::thread::spawn(|| {
        assert!(
            Presenter::new(640, 360)
                .err()
                .expect("Worker presenter unexpectedly created")
                .contains("main thread")
        );
    })
    .join()
    .map_err(|_| "Main-thread rejection check failed")?;
    let mut presenter = Presenter::new(width as u32, height as u32)?;
    let mut previous = 0.0;
    for frame in 0..2 {
        let mut tiled = vec![0; video_tile::required_bytes(width as u32, height as u32)?];
        let mut expected = Vec::with_capacity(width * height * 4);
        for y in 0..height {
            for x in 0..width {
                let index = ((y % 128) * 512 + x % 512) * 2;
                let within =
                    u16::from_le_bytes(table[index..index + 2].try_into().unwrap()) as usize;
                let destination = ((x / 512) * 65536 + (y / 128) * (128 * width) + within) * 4;
                let pixel = if (x / 40 + y / 40 + frame) % 2 == 0 {
                    [255, 64, 16, 255]
                } else {
                    [16, 128, 255, 255]
                };
                tiled[destination..destination + 4].copy_from_slice(&pixel);
                expected.extend(pixel);
            }
        }
        let rgba = video_tile::untile_rgba(&tiled, width as u32, height as u32)?;
        assert_eq!(rgba, expected);
        let evidence = presenter.present(&rgba)?;
        if evidence.presented_time <= previous {
            return Err("Presentation clock did not advance".into());
        }
        previous = evidence.presented_time;
        println!(
            "PASS: actual ELF tile-table fixture -> Rust untile -> visible Metal frame {frame}: {evidence:?}"
        );
    }
    presenter.close()?;
    presenter.close()?;
    assert!(presenter.present(&vec![0; width * height * 4]).is_err());
    println!("Synthetic frames only; guest VideoOut dispatch and gameplay remain unimplemented");
    Ok(())
}
