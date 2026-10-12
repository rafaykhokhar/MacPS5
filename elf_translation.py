"""Translate the bounded entry function of a restricted static x86-64 ELF."""
import argparse
import pathlib
import struct
import unittest

from prototype import translate

HEADER = struct.Struct("<16sHHIQQQIHHHHHH")
SEGMENT = struct.Struct("<IIQQQQQQ")


def entry_function(data: bytes, size: int) -> bytes:
    if size <= 0 or size > 65536:
        raise ValueError("Entry function size must be between 1 and 65536 bytes")
    if len(data) < HEADER.size:
        raise ValueError("Truncated ELF header")
    ident, kind, machine, version, entry, phoff, shoff, flags, ehsize, phsize, phnum, shsize, shnum, shstr = HEADER.unpack_from(data)
    if ident[:7] != b"\x7fELF\x02\x01\x01" or version != 1:
        raise ValueError("Requires little-endian ELF64 version 1")
    if kind != 2 or machine != 62:
        raise ValueError("Requires an x86-64 ET_EXEC executable")
    if flags != 0 or ehsize != HEADER.size or phsize != SEGMENT.size or phnum == 0:
        raise ValueError("Unsupported ELF header layout")
    if phoff < HEADER.size or phoff + phsize * phnum > len(data):
        raise ValueError("Program headers are outside the file")
    if shnum or shoff:
        raise ValueError("Section tables are not supported in this restricted milestone")
    loads = []
    for index in range(phnum):
        ptype, pflags, offset, address, physical, filesz, memsz, align = SEGMENT.unpack_from(data, phoff + index * phsize)
        if ptype not in (0, 1):
            raise ValueError("Dynamic linking and other program-header types are unsupported")
        if ptype == 0:
            continue
        if filesz > memsz or offset + filesz > len(data) or address + memsz > (1 << 64):
            raise ValueError("Invalid load-segment bounds")
        if align not in (0, 1) and (align & (align - 1) or address % align != offset % align):
            raise ValueError("Invalid segment alignment")
        if pflags & ~7 or (pflags & 3) == 3:
            raise ValueError("Unsupported segment permissions")
        for prior_address, prior_end, *_ in loads:
            if address < prior_end and prior_address < address + memsz:
                raise ValueError("Overlapping load segments")
        loads.append((address, address + memsz, offset, filesz, pflags))
    for address, end, offset, filesz, pflags in loads:
        if address <= entry < end:
            if pflags & 5 != 5:
                raise ValueError("Entry segment must be readable and executable")
            displacement = entry - address
            if displacement + size > filesz:
                raise ValueError("Entry function extends outside file-backed segment")
            return data[offset + displacement:offset + displacement + size]
    raise ValueError("Entry point is not in a load segment")


def fixture(code: bytes) -> bytes:
    offset = 0x1000
    address = 0x401000
    header = HEADER.pack(b"\x7fELF\x02\x01\x01" + bytes(9), 2, 62, 1, address,
                         HEADER.size, 0, 0, HEADER.size, SEGMENT.size, 1, 0, 0, 0)
    segment = SEGMENT.pack(1, 5, offset, address, address, len(code), len(code), 0x1000)
    return (header + segment).ljust(offset, b"\x00") + code


class ElfTests(unittest.TestCase):
    def test_extract_and_translate(self):
        code = bytes.fromhex("89 f8 01 f0 c3")
        self.assertEqual(entry_function(fixture(code), len(code)), code)
        self.assertIn("add w9, w9, w1", translate(entry_function(fixture(code), len(code)), "elf_add"))

    def test_bad_headers(self):
        good = fixture(bytes.fromhex("89 f8 c3"))
        for offset, value in [(0, 0), (4, 1), (5, 2), (16, 3), (18, 183), (20, 2), (52, 0), (54, 0)]:
            with self.subTest(offset=offset):
                changed = bytearray(good)
                changed[offset] = value
                with self.assertRaises(ValueError):
                    entry_function(changed, 3)

    def test_bad_segments(self):
        good = fixture(bytes.fromhex("89 f8 c3"))
        mutations = [(64, "I", 2), (68, "I", 7), (72, "Q", len(good) + 1),
                     (80, "Q", 0), (96, "Q", 100), (104, "Q", 1), (112, "Q", 3)]
        for offset, fmt, value in mutations:
            with self.subTest(offset=offset):
                changed = bytearray(good)
                struct.pack_into("<" + fmt, changed, offset, value)
                with self.assertRaises(ValueError):
                    entry_function(changed, 3)

    def test_bounds(self):
        good = fixture(bytes.fromhex("89 f8 c3"))
        for data, size in [(b"", 3), (good[:80], 3), (good[:-1], 3), (good, 4), (good, 0), (good, 65537)]:
            with self.assertRaises(ValueError):
                entry_function(data, size)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--test", action="store_true")
    parser.add_argument("--fixture", type=pathlib.Path)
    parser.add_argument("--input", type=pathlib.Path)
    parser.add_argument("--size", type=int)
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--symbol", default="elf_add")
    args = parser.parse_args()
    if args.test:
        unittest.main(argv=["elf_translation"], exit=True)
    elif args.fixture:
        args.fixture.write_bytes(fixture(bytes.fromhex("89 f8 01 f0 c3")))
    elif args.input and args.output and args.size:
        args.output.write_text(translate(entry_function(args.input.read_bytes(), args.size), args.symbol))
    else:
        parser.error("Use --test, --fixture FILE, or --input ELF --size N --output ASM")
