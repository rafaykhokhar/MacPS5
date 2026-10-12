"""Restricted x86-64 leaf-function to ARM64 assembly proof of concept."""
import argparse
import pathlib
import subprocess


def translate(code: bytes, symbol: str) -> str:
    if not symbol.isidentifier() or not symbol.isascii():
        raise ValueError("Invalid symbol")
    decoded = {}
    offset = 0
    while offset < len(code):
        start = offset
        pair = code[offset:offset + 2]
        if pair == b"\x89\xf8":
            instruction = ("mov", ["mov w9, w0"], None)
            offset += 2
        elif code[offset] == 0xB8:
            if offset + 5 > len(code):
                raise ValueError("Truncated mov immediate")
            value = int.from_bytes(code[offset + 1:offset + 5], "little")
            instruction = ("mov", [f"movz w9, #{value & 65535}", f"movk w9, #{value >> 16}, lsl #16"], None)
            offset += 5
        elif pair in (b"\x01\xf0", b"\x29\xf0"):
            op = "add" if pair == b"\x01\xf0" else "sub"
            instruction = ("arithmetic", [f"{op} w9, w9, w1"], None)
            offset += 2
        elif pair == b"\x39\xf7":
            instruction = ("cmp", ["cmp w0, w1"], None)
            offset += 2
        elif code[offset:offset + 3] in (b"\x0f\x42\xc6", b"\x0f\x47\xc6", b"\x0f\x4c\xc6", b"\x0f\x4f\xc6"):
            condition = {0x42: "lo", 0x47: "hi", 0x4C: "lt", 0x4F: "gt"}[code[offset + 1]]
            instruction = ("cmov", [f"csel w9, w1, w9, {condition}"], None)
            offset += 3
        elif pair == b"\x8b\x07":
            instruction = ("mov", ["ldr w9, [x0]"], None)
            offset += 2
        elif pair == b"\x89\x37":
            instruction = ("store", ["str w1, [x0]"], None)
            offset += 2
        elif code[offset] in (0x72, 0x73, 0x74, 0x75, 0x76, 0x77, 0x7C, 0x7D, 0x7E, 0x7F, 0xEB):
            if offset + 2 > len(code):
                raise ValueError("Truncated short branch")
            opcode = code[offset]
            target = offset + 2 + int.from_bytes(code[offset + 1:offset + 2], "little", signed=True)
            if target <= offset:
                raise ValueError("Backward branches are not supported")
            conditions = {0x72: "lo", 0x73: "hs", 0x74: "eq", 0x75: "ne", 0x76: "ls", 0x77: "hi", 0x7C: "lt", 0x7D: "ge", 0x7E: "le", 0x7F: "gt"}
            op = "b" if opcode == 0xEB else "b." + conditions[opcode]
            instruction = ("jump" if opcode == 0xEB else "branch", [f"{op} .L{symbol}_{target}"], target)
            offset += 2
        elif code[offset] == 0xC3:
            instruction = ("ret", ["mov w0, w9", "ret"], None)
            offset += 1
        else:
            raise ValueError(f"Unsupported instruction at byte {offset}: {code[offset]:02x}")
        decoded[start] = (offset, *instruction)
    if not decoded:
        raise ValueError("Empty function")
    for end, kind, assembly, target in decoded.values():
        if target is not None and target not in decoded:
            raise ValueError("Branch target is not an instruction boundary")
    pending = [(0, False, False)]
    visited = set()
    while pending:
        address, initialized, comparison = pending.pop()
        state = (address, initialized, comparison)
        if state in visited:
            continue
        visited.add(state)
        if address not in decoded:
            raise ValueError("Control flow falls outside function")
        end, kind, assembly, target = decoded[address]
        if kind in ("arithmetic", "cmov", "ret") and not initialized:
            raise ValueError("Read of undefined EAX on a reachable path")
        if kind in ("cmov", "branch") and not comparison:
            raise ValueError("Condition requires supported comparison flags on every path")
        if kind == "mov":
            initialized = True
        if kind == "cmp":
            comparison = True
        if kind == "arithmetic":
            comparison = False
        if kind == "ret":
            continue
        if target is not None:
            pending.append((target, initialized, comparison))
        if kind != "jump":
            pending.append((end, initialized, comparison))
    reachable = {state[0] for state in visited}
    if reachable != set(decoded):
        raise ValueError("Unreachable instructions are not supported")
    lines = [".text", ".p2align 2", f".globl _{symbol}", f"_{symbol}:"]
    for start, (end, kind, assembly, target) in decoded.items():
        lines.extend([f".L{symbol}_{start}:", *assembly])
    return "\n".join(lines) + "\n"


def verify(directory: pathlib.Path):
    directory.mkdir(parents=True, exist_ok=True)
    fixtures = {
        "guest_add": bytes.fromhex("89 f8 01 f0 c3"),
        "guest_sub": bytes.fromhex("89 f8 29 f0 c3"),
        "guest_identity": bytes.fromhex("89 f8 c3"),
        "guest_constant": bytes.fromhex("b8 ef be ad de c3"),
        "guest_umax": bytes.fromhex("39 f7 89 f8 0f 42 c6 c3"),
        "guest_umin": bytes.fromhex("39 f7 89 f8 0f 47 c6 c3"),
        "guest_smax": bytes.fromhex("39 f7 89 f8 0f 4c c6 c3"),
        "guest_smin": bytes.fromhex("39 f7 89 f8 0f 4f c6 c3"),
        "guest_branch": bytes.fromhex("39 f7 72 06 b8 02 00 00 00 c3 b8 01 00 00 00 c3"),
    }
    assembly = directory / "translated.S"
    branch_opcodes = (0x72, 0x73, 0x74, 0x75, 0x76, 0x77, 0x7C, 0x7D, 0x7E, 0x7F)
    for opcode in branch_opcodes:
        fixtures[f"branch_{opcode:02x}"] = bytes([0x39, 0xF7, opcode, 6]) + bytes.fromhex("b8 02 00 00 00 c3 b8 01 00 00 00 c3")
    assembly.write_text("\n".join(translate(code, name) for name, code in fixtures.items())
                       + translate(bytes.fromhex("8b 07 c3"), "guest_load")
                       + translate(bytes.fromhex("89 37 8b 07 c3"), "guest_store"))
    rejected = [b"", bytes.fromhex("01 f0 c3"), bytes.fromhex("b8 01"),
                bytes.fromhex("89 f8"), bytes.fromhex("89 f8 c3 90"),
                bytes.fromhex("89 f8 74 00 c3"), bytes.fromhex("89 f8 90 c3"),
                bytes.fromhex("89 f8 0f 42 c6 c3"),
                bytes.fromhex("39 f7 0f 42 c6 c3"),
                bytes.fromhex("39 f7 89 f8 01 f0 0f 42 c6 c3"),
                bytes.fromhex("39 f7 72 ff b8 00 00 00 00 c3"),
                bytes.fromhex("39 f7 72 01 b8 00 00 00 00 c3"),
                bytes.fromhex("39 f7 72 00 c3"),
                bytes.fromhex("39 f7 72"),
                bytes.fromhex("89 f8 eb 00"),
                bytes.fromhex("89 f8 72 00 c3")]
    for code in rejected:
        try:
            translate(code, "invalid")
        except ValueError:
            continue
        raise AssertionError(f"Accepted unsupported input {code.hex()}")
    harness = directory / "verify.cpp"
    harness_text = '''#include <cstdint>
#include <iostream>
#include <algorithm>
#include <bit>
#if !defined(__aarch64__) || !defined(__APPLE__)
#error This verification requires native ARM64 macOS
#endif
extern "C" uint32_t guest_add(uint32_t, uint32_t);
extern "C" uint32_t guest_sub(uint32_t, uint32_t);
extern "C" uint32_t guest_identity(uint32_t, uint32_t);
extern "C" uint32_t guest_constant(uint32_t, uint32_t);
extern "C" uint32_t guest_umax(uint32_t, uint32_t);
extern "C" uint32_t guest_umin(uint32_t, uint32_t);
extern "C" uint32_t guest_smax(uint32_t, uint32_t);
extern "C" uint32_t guest_smin(uint32_t, uint32_t);
extern "C" uint32_t guest_branch(uint32_t, uint32_t);
extern "C" uint32_t guest_load(const uint32_t*);
extern "C" uint32_t guest_store(uint32_t*, uint32_t);
BRANCH_DECLARATIONS
int main() {
    uint32_t state = 0x12345678;
    auto next = [&]() { state ^= state << 13; state ^= state >> 17; state ^= state << 5; return state; };
    auto check = [](uint32_t a, uint32_t b) {
        auto sa = std::bit_cast<int32_t>(a), sb = std::bit_cast<int32_t>(b);
        bool conditions[] = {a < b, a >= b, a == b, a != b, a <= b, a > b, sa < sb, sa >= sb, sa <= sb, sa > sb};
        uint32_t (*branches[])(uint32_t,uint32_t) = {BRANCH_NAMES};
        for (int i = 0; i < 10; ++i) if (branches[i](a,b) != (conditions[i] ? 1u : 2u)) return false;
        return guest_add(a,b) == uint32_t(a+b) && guest_sub(a,b) == uint32_t(a-b)
            && guest_identity(a,b) == a && guest_constant(a,b) == 0xdeadbeef
            && guest_umax(a,b) == std::max(a,b) && guest_umin(a,b) == std::min(a,b)
            && guest_smax(a,b) == std::bit_cast<uint32_t>(std::max(std::bit_cast<int32_t>(a),std::bit_cast<int32_t>(b)))
            && guest_smin(a,b) == std::bit_cast<uint32_t>(std::min(std::bit_cast<int32_t>(a),std::bit_cast<int32_t>(b)))
            && guest_branch(a,b) == (a < b ? 1u : 2u);
    };
    uint32_t edges[] = {0, 1, 0x7fffffff, 0x80000000, 0xffffffff};
    for (auto a : edges) for (auto b : edges) if (!check(a,b)) return 1;
    for (int i = 0; i < 100000; ++i) { auto a = next(); auto b = next(); if (!check(a,b)) return 1; }
    for (int i = 0; i < 100000; ++i) {
        uint32_t cell = next(); if (guest_load(&cell) != cell) return 2;
        uint32_t value = next(); if (guest_store(&cell, value) != value || cell != value) return 3;
    }
    std::cout << "PASS: 2100475 native ARM64 checks (arithmetic, comparisons, all supported branches, memory); no Rosetta\\n";
}
'''
    harness_text = harness_text.replace("BRANCH_DECLARATIONS", "\n".join(f'extern "C" uint32_t branch_{opcode:02x}(uint32_t,uint32_t);' for opcode in branch_opcodes))
    harness_text = harness_text.replace("BRANCH_NAMES", ",".join(f"branch_{opcode:02x}" for opcode in branch_opcodes))
    harness.write_text(harness_text)
    binary = directory / "verify-arm64"
    subprocess.run(["xcrun", "clang++", "-arch", "arm64", "-std=c++20", "-O2",
                    str(harness), str(assembly), "-o", str(binary)], check=True)
    subprocess.run(["file", str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
    print(f"PASS: {len(rejected)} invalid/unsupported inputs rejected")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verify", type=pathlib.Path)
    parser.add_argument("--input", type=pathlib.Path)
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--symbol", default="guest_function")
    args = parser.parse_args()
    if args.verify:
        verify(args.verify)
    elif args.input and args.output:
        args.output.write_text(translate(args.input.read_bytes(), args.symbol))
    else:
        parser.error("Use --verify DIR, or --input FILE --output FILE")
