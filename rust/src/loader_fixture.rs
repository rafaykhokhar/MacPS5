//! Synthetic qualified AGC import ELF; metadata layout matches the retained oracle.
use sha1::{Digest, Sha1};
fn put(data: &mut [u8], offset: usize, value: u64) {
    data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
pub fn graphics(sce: bool) -> Vec<u8> {
    let mut data = vec![0; 0x1008];
    data[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    for (offset, value) in [(16, 3u16), (18, 62), (52, 64), (54, 56), (56, 3)] {
        data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }
    data[20..24].copy_from_slice(&1u32.to_le_bytes());
    put(&mut data, 24, 0x100);
    put(&mut data, 32, 64);
    for (offset, values) in [
        (64, [0x500000001, 0, 0, 0, 0x1000, 0x1000, 0x1000]),
        (120, [0x600000001, 0x1000, 0x4000, 0, 8, 0x1000, 0x1000]),
    ] {
        for (i, value) in values.iter().enumerate() {
            put(&mut data, offset + i * 8, *value);
        }
    }
    data[0x100..0x10a].copy_from_slice(&[0xff, 0x15, 0xfa, 0x3e, 0, 0, 0x89, 0xc3, 0xeb, 0x36]);
    data[0x140..0x142].copy_from_slice(&[0x0f, 0x0b]);
    let mut hash = Sha1::new();
    hash.update(b"sceAgcDriverSubmitDcb");
    hash.update([
        0x51, 0x8d, 0x64, 0xa6, 0x35, 0xde, 0xd8, 0xc1, 0xe6, 0xb0, 0x39, 0xb1, 0xc3, 0xe5, 0x52,
        0x30,
    ]);
    let digest = hash.finalize();
    let value = u64::from_le_bytes(digest[..8].try_into().unwrap());
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+-";
    let mut name = Vec::new();
    for i in 0..11 {
        let index = if i == 10 {
            (value & 15) << 2
        } else {
            (value >> (58 - i * 6)) & 63
        };
        name.push(alphabet[index as usize]);
    }
    name.extend_from_slice(b"#A#B\0");
    let mut strings = vec![0];
    strings.extend_from_slice(&name);
    let needed = strings.len() as u64;
    strings.extend_from_slice(b"libSceAgcDriver.prx\0");
    let module = strings.len() as u64;
    strings.extend_from_slice(b"libSceAgcDriver\0");
    data[0x380..0x380 + strings.len()].copy_from_slice(&strings);
    let tags = [
        (7, 0x480),
        (8, 24),
        (9, 24),
        (6, 0x300),
        (11, 24),
        (5, 0x380),
        (10, strings.len() as u64),
        (1, needed),
        (0x61000045, (1 << 48) | module),
        (0, 0),
    ];
    for (i, (tag, value)) in tags.iter().enumerate() {
        put(&mut data, 0x180 + i * 16, *tag);
        put(&mut data, 0x188 + i * 16, *value);
    }
    for (i, value) in [0x400000002, 0x180, 0x180, 0, 160, 160, 8]
        .iter()
        .enumerate()
    {
        put(&mut data, 176 + i * 8, *value);
    }
    data[0x318..0x31c].copy_from_slice(&1u32.to_le_bytes());
    data[0x31c] = 0x12;
    put(&mut data, 0x480, 0x4000);
    put(&mut data, 0x488, (1 << 32) | 7);
    if sce {
        data.resize(0x1c00, 0);
        data.copy_within(64..64 + 3 * 56, 0x500);
        put(&mut data, 32, 0x500);
        data[56..58].copy_from_slice(&4u16.to_le_bytes());
        for (i, value) in [0x461000000, 0x1800, 0, 0, 0x200, 0x200, 8]
            .iter()
            .enumerate()
        {
            put(&mut data, 0x500 + 3 * 56 + i * 8, *value);
        }
        data.copy_within(0x300..0x330, 0x1800);
        data.copy_within(0x380..0x380 + strings.len(), 0x1840);
        data.copy_within(0x480..0x498, 0x1900);
        data[0x300..0x498].fill(0);
        let tags = [
            (0x61000029, 0x100),
            (0x6100002d, 24),
            (0x6100002b, 7),
            (0x61000039, 0),
            (0x6100003b, 24),
            (0x6100003f, 48),
            (0x61000035, 0x40),
            (0x61000037, strings.len() as u64),
            (1, needed),
            (0x61000045, (1 << 48) | module),
            (0, 0),
        ];
        for (i, (tag, value)) in tags.iter().enumerate() {
            put(&mut data, 0x180 + i * 16, *tag);
            put(&mut data, 0x188 + i * 16, *value);
        }
        put(&mut data, 0x500 + 2 * 56 + 32, 176);
        put(&mut data, 0x500 + 2 * 56 + 40, 176);
    }
    data
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::Image;
    #[test]
    fn duplicate_module_ids_fail_before_commit() {
        for sce in [false, true] {
            let mut bytes = graphics(sce);
            let end = if sce { 0x220 } else { 0x210 };
            let module_value = u64::from_le_bytes(bytes[end - 8..end].try_into().unwrap());
            put(&mut bytes, end, 0x61000045);
            put(&mut bytes, end + 8, module_value);
            let dynamic = if sce { 0x500 + 2 * 56 } else { 176 };
            let size = if sce { 192 } else { 176 };
            put(&mut bytes, dynamic + 32, size);
            put(&mut bytes, dynamic + 40, size);
            let mut image = Image::load(&bytes, 0x100000, 65536).unwrap();
            let before = image
                .segments
                .iter()
                .map(|s| s.bytes.clone())
                .collect::<Vec<_>>();
            assert!(image.relocate_scoped(|_| Some(1)).is_err());
            assert!(image.relocations.is_empty());
            assert!(image.unresolved.is_empty());
            assert_eq!(
                image
                    .segments
                    .iter()
                    .map(|s| s.bytes.clone())
                    .collect::<Vec<_>>(),
                before
            );
        }
    }
    #[test]
    fn both_metadata_families_preserve_scope_and_missing_records() {
        for sce in [false, true] {
            let bytes = graphics(sce);
            let mut image = Image::load(&bytes, 0x100000, 65536).unwrap();
            image
                .relocate_scoped(|identity| {
                    assert_eq!(identity.module, "libSceAgcDriver.prx");
                    assert_eq!(identity.module_id, Some(1));
                    assert_eq!(identity.library_token, "A");
                    None
                })
                .unwrap();
            assert_eq!(image.unresolved.len(), 1);
            assert_eq!(image.unresolved[0].identity.module_id, Some(1));
            assert_eq!(image.applied, 0);
            let mut image = Image::load(&bytes, 0x100000, 65536).unwrap();
            image.relocate_scoped(|_| Some(0x123456)).unwrap();
            assert_eq!(image.relocations, [(0x104000, 0x123456)]);
        }
    }
}
