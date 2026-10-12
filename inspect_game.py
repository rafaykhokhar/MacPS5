"""Read-only ELF dependency inventory; never loads or executes guest code."""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import struct


def inspect(data):
    def take(offset, size):
        if offset < 0 or size < 0 or offset + size > len(data):
            raise ValueError("ELF structure outside file bounds")
        return data[offset:offset + size]

    def unpack(fmt, offset):
        return struct.unpack(fmt, take(offset, struct.calcsize(fmt)))

    h = unpack("<16sHHIQQQIHHHHHH", 0)
    if h[0][:7] != b"\x7fELF\x02\x01\x01" or h[2] != 62 or h[3] != 1:
        raise ValueError("Requires little-endian x86-64 ELF version 1")
    if h[8] != 64 or h[9] != 56 or (h[12] and h[11] != 64):
        raise ValueError("Unsupported ELF table layout")
    take(h[5], h[9] * h[10])
    take(h[6], h[11] * h[12])
    segments = []
    entry_bytes = None
    for i in range(h[10]):
        kind, flags, offset, address, physical, filesz, memsz, alignment = unpack("<IIQQQQQQ", h[5] + i * h[9])
        take(offset, filesz)
        if kind != 1:
            continue
        if filesz > memsz or address + memsz > 1 << 64:
            raise ValueError("Invalid load segment")
        segments.append(dict(address=hex(address), file_bytes=filesz, memory_bytes=memsz, flags=flags, alignment=alignment))
        if flags & 1 and address <= h[4] < address + filesz:
            entry_bytes = take(offset + h[4] - address, min(64, address + filesz - h[4])).hex()
    sections = [unpack("<IIQQQQIIQQ", h[6] + i * 64) for i in range(h[12])]
    imports = set()
    relocations = Counter()
    for section in sections:
        _, kind, _, _, offset, size, link, _, _, stride = section
        if kind != 8:
            take(offset, size)
        if kind == 11:
            if stride != 24 or size % stride or link >= len(sections) or sections[link][1] != 3:
                raise ValueError("Invalid dynamic symbol table")
            strings = take(sections[link][4], sections[link][5])
            for index in range(size // stride):
                name, info, other, defined, value, symbol_size = unpack("<IBBHQQ", offset + index * stride)
                if defined or not name:
                    continue
                end = strings.find(b"\0", name)
                if name >= len(strings) or end < 0:
                    raise ValueError("Invalid dynamic symbol name")
                imports.add(strings[name:end].decode("utf-8"))
        elif kind == 4:
            if stride != 24 or size % stride:
                raise ValueError("Invalid RELA table")
            for index in range(size // stride):
                target, info, addend = unpack("<QQq", offset + index * stride)
                relocations[str(info & 0xffffffff)] += 1
    return {
        "sha256": hashlib.sha256(data).hexdigest(),
        "file_bytes": len(data), "elf_type": h[1], "entry": hex(h[4]),
        "load_segments": segments,
        "segment_memory_bytes": sum(segment["memory_bytes"] for segment in segments),
        "entry_first_64_bytes": entry_bytes,
        "undefined_dynamic_symbols": sorted(imports),
        "undefined_dynamic_symbol_count": len(imports),
        "relocation_counts_by_x86_type": dict(sorted(relocations.items())),
        "execution_tested": False,
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = inspect(args.input.read_bytes())
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(f"ELF type {result['elf_type']}; {len(result['load_segments'])} load segments; {result['undefined_dynamic_symbol_count']} undefined dynamic symbols")
    print("Relocations:", result["relocation_counts_by_x86_type"])
    print("Read-only inspection complete; guest execution not attempted.")
