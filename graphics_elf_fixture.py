"""Synthetic x86-64 ET_DYN fixture calling an imported AGC submission service.

This is an ABI integration fixture, not a PS5 game or a console executable dump.
"""
import struct
import hashlib
import base64

ENTRY = 0x100
CODE = bytes.fromhex("ff 15 fa 3e 00 00 89 c3 eb 36")


def nid(name):
    digest = hashlib.sha1(name.encode() + bytes.fromhex("518d64a635ded8c1e6b039b1c3e55230")).digest()
    return base64.b64encode(digest[:8][::-1], altchars=b"+-").decode().rstrip("=")


def build(qualified=True):
    data = bytearray(0x1008)
    identity = b"\x7fELF\x02\x01\x01" + bytes(9)
    struct.pack_into("<16sHHIQQQIHHHHHH", data, 0, identity, 3, 62, 1, ENTRY, 64, 0, 0, 64, 56, 3, 0, 0, 0)
    # Separate executable text and writable import table at guest VA 0x4000.
    struct.pack_into("<IIQQQQQQ", data, 64, 1, 5, 0, 0, 0, 0x1000, 0x1000, 0x1000)
    struct.pack_into("<IIQQQQQQ", data, 120, 1, 6, 0x1000, 0x4000, 0, 8, 0x1000, 0x1000)
    data[ENTRY:ENTRY + len(CODE)] = CODE
    data[0x140:0x142] = bytes.fromhex("0f 0b")  # Untranslated UD2 fixture-stop marker.
    name = ((nid("sceAgcDriverSubmitDcb") + "#A#B") if qualified else "sceAgcDriverSubmitDcb").encode() + b"\0"
    dependency = b"libSceAgcDriver.prx\0"
    strings = b"\0" + name + dependency
    module_offset = len(strings)
    strings += b"libSceAgcDriver\0"
    data[0x380:0x380 + len(strings)] = strings
    tags = [(7, 0x480), (8, 24), (9, 24), (6, 0x300), (11, 24),
            (5, 0x380), (10, len(strings)), (1, 1 + len(name))]
    if qualified:
        tags.append((0x61000045, (1 << 48) | module_offset))
    tags.append((0, 0))
    struct.pack_into("<IIQQQQQQ", data, 176, 2, 4, 0x180, 0x180, 0, len(tags)*16, len(tags)*16, 8)
    for index, (tag, value) in enumerate(tags):
        struct.pack_into("<qQ", data, 0x180 + index * 16, tag, value)
    struct.pack_into("<IBBHQQ", data, 0x318, 1, 0x12, 0, 0, 0, 0)
    struct.pack_into("<QQq", data, 0x480, 0x4000, (1 << 32) | 7, 0)
    return bytes(data)


def build_sce():
    """Same guest code with SCE-relative metadata outside every PT_LOAD."""
    data = bytearray(build())
    data.extend(bytes(0x1c00-len(data)))
    headers = bytes(data[64:64+3*56])
    data[0x500:0x500+len(headers)] = headers
    struct.pack_into("<Q", data, 32, 0x500)
    struct.pack_into("<H", data, 56, 4)
    struct.pack_into("<IIQQQQQQ", data, 0x500+3*56,
                     0x61000000, 4, 0x1800, 0, 0, 0x200, 0x200, 8)
    # File-backed metadata only: these bytes are deliberately not guest memory.
    string_size = struct.unpack_from("<Q", data, 0x1e8)[0]
    needed_offset = struct.unpack_from("<Q", data, 0x1f8)[0]
    module_record = struct.unpack_from("<Q", data, 0x208)[0]
    data[0x1800:0x1830] = data[0x300:0x330]
    data[0x1840:0x1840+string_size] = data[0x380:0x380+string_size]
    data[0x1900:0x1918] = data[0x480:0x498]
    data[0x300:0x498] = bytes(0x198)
    # Use the PLT relocation family as well as the SCE symbol/string tables.
    tags = [(0x61000029, 0x100), (0x6100002d, 24), (0x6100002b, 7),
            (0x61000039, 0), (0x6100003b, 24), (0x6100003f, 48),
            (0x61000035, 0x40), (0x61000037, string_size),
            (1, needed_offset), (0x61000045, module_record), (0, 0)]
    for index, (tag, value) in enumerate(tags):
        struct.pack_into("<qQ", data, 0x180+index*16, tag, value)
    struct.pack_into("<QQ", data, 0x500+2*56+32, len(tags)*16, len(tags)*16)
    return bytes(data)
