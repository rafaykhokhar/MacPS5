"""Ahead-of-time ARM64 translation of an x86-64 startup prefix ending at a call.

This uses explicit guest registers and guarded memory access. Unsupported
instructions fail translation. Calls stop before their architectural effects.
"""
import argparse
from dataclasses import dataclass
import hashlib
import json
from pathlib import Path
import struct


@dataclass
class Operand:
    kind: str
    value: int = 0
    base: int = -1
    index: int = -1
    scale: int = 1
    displacement: int = 0
    rip: bool = False
    bit_offset: int = 0


@dataclass
class Instruction:
    address: int
    end: int
    kind: str
    bits: int = 64
    dst: Operand = None
    src: Operand = None
    condition: int = 0
    target: int = 0
    aligned: bool = False
    source_bits: int = 0
    control: Operand = None


class Decoder:
    def __init__(self, code, address):
        self.code, self.address, self.position = code, address, 0

    def number(self, count, signed=False):
        if self.position + count > len(self.code):
            raise ValueError("Truncated x86 instruction")
        value = int.from_bytes(self.code[self.position:self.position + count], "little", signed=signed)
        self.position += count
        return value

    def register(self,index,bits,rex):
        if bits==8 and not rex and 4<=index<8:
            return Operand('reg',index-4,bit_offset=8)
        return Operand('reg',index)

    def operands(self, rex, bits, register_bits=None):
        modrm = self.number(1)
        mode, reg, rm = modrm >> 6, (modrm >> 3) & 7, modrm & 7
        register = self.register(reg + (8 if rex & 4 else 0),bits if register_bits is None else register_bits,rex)
        if mode == 3:
            return reg, register, self.register(rm + (8 if rex & 1 else 0),bits,rex)
        memory = Operand("mem")
        if rm == 4:
            sib = self.number(1)
            memory.scale = 1 << (sib >> 6)
            index, base = (sib >> 3) & 7, sib & 7
            if index != 4 or rex & 2:
                memory.index = index + (8 if rex & 2 else 0)
            if mode == 0 and base == 5:
                memory.displacement = self.number(4, True)
            else:
                memory.base = base + (8 if rex & 1 else 0)
        elif mode == 0 and rm == 5:
            memory.rip = True
            memory.displacement = self.number(4, True)
        else:
            memory.base = rm + (8 if rex & 1 else 0)
        if mode == 1:
            memory.displacement = self.number(1, True)
        elif mode == 2:
            memory.displacement = self.number(4, True)
        return reg, register, memory

    def next(self):
        start = self.address + self.position
        opcode = self.number(1)
        padding_prefix=False
        operand_prefix=False
        segment_prefix=False
        while opcode in (0x66,0x2e):
            padding_prefix=True
            operand_prefix |= opcode==0x66
            segment_prefix |= opcode==0x2e
            opcode=self.number(1)
        address_prefix = opcode == 0x67
        if address_prefix:
            opcode = self.number(1)
        rex = 0
        if 0x40 <= opcode <= 0x4F:
            rex = opcode
            opcode = self.number(1)
        bits = 64 if rex & 8 else 32
        instruction = Instruction(start, 0, "", bits)
        if padding_prefix and opcode not in (0x0f,0x90):
            raise ValueError("Legacy prefixes currently supported only for NOP")
        if address_prefix and opcode != 0xE8:
            raise ValueError("Address-size prefix only supported for direct calls")
        if opcode in (0xC4,0xC5):
            if rex or address_prefix:
                raise ValueError("Unsupported prefix before VEX")
            vex=self.number(1)
            vector_rex=((~vex>>7)&1)<<2
            opcode_map=1
            if opcode==0xC4:
                opcode_map=vex&31
                vector_rex|=((~vex>>6)&1)<<1 | ((~vex>>5)&1)
                vex=self.number(1)
            vector_opcode=self.number(1)
            if opcode_map==1 and vector_opcode==0x57 and vex&3 in (0,1):
                _,instruction.dst,instruction.src=self.operands(vector_rex,64)
                instruction.dst.kind='vector'
                if instruction.src.kind=='reg': instruction.src.kind='vector'
                instruction.control=Operand('vector',(~vex>>3)&15)
                instruction.bits=256 if vex&4 else 128
                instruction.kind='vector_xor'
            elif opcode_map==3 and vector_opcode==0xf0 and vex&0x7f==0x7b:
                instruction.bits=64 if vex&0x80 else 32
                _,instruction.dst,instruction.src=self.operands(vector_rex,instruction.bits)
                instruction.control=Operand('imm',self.number(1))
                instruction.kind='rorx'
            elif opcode_map==2 and vector_opcode==0xf6 and vex&7==3:
                instruction.bits=64 if vex&0x80 else 32
                _,instruction.dst,instruction.src=self.operands(vector_rex,instruction.bits)
                instruction.control=Operand('reg',(~vex>>3)&15)
                instruction.kind='mulx'
            elif opcode_map==2 and vector_opcode==0xf2 and not vex&7:
                instruction.bits=64 if vex&0x80 else 32
                _,instruction.dst,instruction.src=self.operands(vector_rex,instruction.bits)
                instruction.control=Operand('reg',(~vex>>3)&15)
                instruction.kind='andn'
            elif opcode_map==2 and vector_opcode==0xf7 and not vex&4:
                instruction.bits=64 if vex&0x80 else 32
                _,instruction.dst,instruction.src=self.operands(vector_rex,instruction.bits)
                instruction.control=Operand('reg',(~vex>>3)&15)
                instruction.kind={0:'bextr',1:'shlx',2:'sarx',3:'shrx'}[vex&3]
            else:
                if opcode_map!=1 or vex&0x78!=0x78 or vex&3:
                    raise ValueError("Invalid or unsupported VEX move prefix")
                if vector_opcode not in (0x10,0x11,0x28,0x29):
                    raise ValueError("Unsupported VEX opcode")
                _,reg,rm=self.operands(vector_rex,64)
                reg.kind="vector"
                if rm.kind=="reg":
                    rm.kind="vector"
                instruction.kind="vector_move"
                instruction.bits=256 if vex&4 else 128
                instruction.aligned=vector_opcode in (0x28,0x29)
                instruction.dst,instruction.src=(rm,reg) if vector_opcode in (0x11,0x29) else (reg,rm)
        elif 0x50 <= opcode <= 0x57:
            instruction.kind, instruction.bits = "push", 64
            instruction.src = Operand("reg", opcode - 0x50 + (8 if rex & 1 else 0))
        elif 0x58 <= opcode <= 0x5F:
            instruction.kind, instruction.bits = "pop", 64
            instruction.dst = Operand("reg", opcode - 0x58 + (8 if rex & 1 else 0))
        elif opcode in (0x38,0x3a):
            _,reg,rm=self.operands(rex,8)
            instruction.kind,instruction.bits='cmp',8
            instruction.dst,instruction.src=(rm,reg) if opcode==0x38 else (reg,rm)
        elif opcode in (0x88,0x8a):
            _,reg,rm=self.operands(rex,8)
            instruction.kind,instruction.bits='mov',8
            instruction.dst,instruction.src=(rm,reg) if opcode==0x88 else (reg,rm)
        elif 0xb0<=opcode<=0xb7:
            instruction.kind,instruction.bits='mov',8
            instruction.dst=self.register(opcode-0xb0+(8 if rex&1 else 0),8,rex)
            instruction.src=Operand('imm',self.number(1))
        elif opcode==0x63:
            if bits!=64:
                raise ValueError('Only 64-bit MOVSXD destination supported')
            _,instruction.dst,instruction.src=self.operands(rex,32,64)
            instruction.kind,instruction.source_bits='sign_extend',32
        elif opcode in (0x08,0x09,0x0a,0x0b,0x20,0x21,0x22,0x23,0x30,0x31,0x32,0x33,0x84,0x85):
            if not opcode&1:
                instruction.bits=bits=8
            _,reg,rm=self.operands(rex,bits)
            instruction.kind='test' if opcode in (0x84,0x85) else {0x08:'or',0x20:'and',0x30:'xor'}[opcode&0xfc]
            instruction.dst,instruction.src=(reg,rm) if opcode&2 else (rm,reg)
        elif opcode in (0x04,0x05,0x2c,0x2d,0x3c,0x3d,0x0c,0x0d,0x24,0x25,0x34,0x35,0xa8,0xa9):
            if not opcode&1:
                instruction.bits=bits=8
            instruction.kind={0x04:'add',0x2c:'sub',0x3c:'cmp',0x0c:'or',0x24:'and',0x34:'xor',0xa8:'test'}[opcode&0xfe]
            instruction.dst=Operand('reg',0)
            instruction.src=Operand('imm',self.number(1 if bits==8 else 4,bits==64))
        elif opcode in (0xf6,0xf7):
            if opcode==0xf6:
                instruction.bits=bits=8
            group,_,instruction.dst=self.operands(rex,bits)
            if group==2:
                instruction.kind='not'
            elif group==0:
                instruction.kind='test'
                instruction.src=Operand('imm',self.number(1 if bits==8 else 4,bits==64))
            else:
                raise ValueError('Unsupported unary arithmetic group')
        elif opcode in (0x89, 0x8B, 0x8D, 0x39, 0x3B, 0x29, 0x2B, 0x01, 0x03):
            _, reg, rm = self.operands(rex, bits)
            instruction.dst, instruction.src = (reg, rm) if opcode in (0x8B, 0x8D, 0x3B, 0x2B, 0x33, 0x03) else (rm, reg)
            instruction.kind = {0x89: "mov", 0x8B: "mov", 0x8D: "lea", 0x39: "cmp", 0x3B: "cmp", 0x29: "sub", 0x2B: "sub", 0x31: "xor", 0x33: "xor", 0x01: "add", 0x03: "add", 0x85: "test"}[opcode]
            if opcode == 0x8D and rm.kind != "mem":
                raise ValueError("LEA requires a memory operand")
        elif opcode in (0xC6, 0xC7):
            immediate_bits = 8 if opcode == 0xC6 else bits
            group, _, rm = self.operands(rex, immediate_bits)
            if group != 0:
                raise ValueError("Unsupported immediate MOV opcode extension")
            instruction.kind, instruction.bits = "mov", immediate_bits
            instruction.dst, instruction.src = rm, Operand("imm", self.number(1 if opcode == 0xC6 else 4, opcode==0xc7 and bits==64))
        elif 0xB8 <= opcode <= 0xBF:
            instruction.kind = "mov"
            instruction.dst = Operand("reg", opcode - 0xB8 + (8 if rex & 1 else 0))
            instruction.src = Operand("imm", self.number(8 if bits == 64 else 4))
        elif opcode in (0x80,0x81, 0x83):
            if opcode==0x80:
                instruction.bits=bits=8
            group, _, rm = self.operands(rex, bits)
            if group not in (0,1,4,5,6,7) or (opcode==0x80 and group not in (1,4,6,7)):
                raise ValueError("Unsupported immediate arithmetic extension")
            instruction.kind = {0:"add",1:'or',4:'and',5:"sub",6:'xor',7:"cmp"}[group]
            instruction.dst, instruction.src = rm, Operand("imm", self.number(1 if opcode in (0x80,0x83) else 4, opcode!=0x80))
        elif opcode in (0xc1,0xd1,0xd3):
            group,_,rm=self.operands(rex,bits)
            kinds={0:'rol',1:'ror',4:'shl',5:'shr',7:'sar'}
            if group not in kinds:
                raise ValueError("Unsupported shift/rotate group")
            instruction.kind,instruction.dst=kinds[group],rm
            instruction.src=Operand('reg',1) if opcode==0xd3 else Operand('imm',self.number(1) if opcode==0xc1 else 1)
        elif opcode == 0xFF:
            group, _, rm = self.operands(rex, bits)
            if group == 0:
                instruction.kind, instruction.dst, instruction.src = "inc", rm, Operand("imm", 1)
            elif group == 2:
                instruction.kind, instruction.bits, instruction.src = "indirect_call", 64, rm
            else:
                raise ValueError("Only INC and indirect CALL implemented for FF group")
        elif 0x70 <= opcode <= 0x7F or opcode == 0xEB:
            displacement = self.number(1, True)
            instruction.kind = "jump" if opcode == 0xEB else "branch"
            instruction.condition = opcode & 15
            instruction.target = self.address + self.position + displacement
        elif opcode in (0xE8,0xE9):
            displacement = self.number(4, True)
            instruction.kind = "call" if opcode==0xE8 else "jump"
            instruction.target = self.address + self.position + displacement
        elif opcode == 0x90 and not (rex & 1):
            instruction.kind = "nop"
        elif opcode == 0x0F:
            extension = self.number(1)
            if padding_prefix and extension!=0x1f and not (extension==0x38 and not segment_prefix):
                raise ValueError("Unsupported prefixed 0F opcode")
            if extension==0x38:
                operation=self.number(1)
                if operation not in (0xf0,0xf1):
                    raise ValueError("Unsupported 0F38 opcode")
                instruction.bits=64 if rex&8 else (16 if operand_prefix else 32)
                _,register,memory=self.operands(rex,instruction.bits)
                if memory.kind!='mem':
                    raise ValueError("MOVBE requires a memory operand")
                instruction.kind='movbe'
                instruction.dst,instruction.src=(register,memory) if operation==0xf0 else (memory,register)
            elif 0x40<=extension<=0x4f:
                _,instruction.dst,instruction.src=self.operands(rex,bits)
                instruction.kind,instruction.condition='cmov',extension&15
            elif 0x90<=extension<=0x9f:
                _,_,instruction.dst=self.operands(rex,8)
                instruction.kind,instruction.bits,instruction.condition='setcc',8,extension&15
            elif 0x80 <= extension <= 0x8F:
                displacement = self.number(4, True)
                instruction.kind, instruction.condition = "branch", extension & 15
                instruction.target = self.address + self.position + displacement
            elif extension in (0xb6,0xb7,0xbe,0xbf):
                instruction.source_bits=8 if extension in (0xb6,0xbe) else 16
                _,instruction.dst,instruction.src=self.operands(rex,instruction.source_bits,bits)
                instruction.kind='zero_extend' if extension in (0xb6,0xb7) else 'sign_extend'
            elif extension != 0x1F:
                raise ValueError(f"Unsupported 0F opcode {extension:02x}")
            else:
                group, _, _ = self.operands(rex, bits)
                if group != 0:
                    raise ValueError("Invalid NOP opcode extension")
                instruction.kind = "nop"
        elif opcode == 0xC3:
            instruction.kind = "ret"
        else:
            raise ValueError(f"Unsupported x86 opcode {opcode:02x} at {start:#x}")
        instruction.end = self.address + self.position
        if instruction.end - start > 15:
            raise ValueError("Overlong instruction")
        return instruction


class Emitter:
    def __init__(self):
        self.lines = []

    def emit(self, *lines):
        self.lines.extend(lines)

    def immediate(self, register, value):
        value &= (1 << 64) - 1
        self.emit(f"movz {register}, #{value & 0xffff}")
        for shift in (16, 32, 48):
            if value >> shift & 0xffff:
                self.emit(f"movk {register}, #{(value >> shift) & 0xffff}, lsl #{shift}")

    def address(self, operand, end):
        if operand.rip:
            self.emit("ldr x1, [x19, #144]")
            self.immediate("x9", end + operand.displacement)
            self.emit("add x1, x1, x9")
            return
        if operand.base >= 0:
            self.emit(f"ldr x1, [x19, #{operand.base * 8}]")
        else:
            self.emit("mov x1, #0")
        if operand.index >= 0:
            self.emit(f"ldr x9, [x19, #{operand.index * 8}]", f"add x1, x1, x9, lsl #{operand.scale.bit_length() - 1}")
        if operand.displacement:
            self.immediate("x9", operand.displacement)
            self.emit("add x1, x1, x9")

    def fault_check(self):
        self.emit("ldr x9, [x19, #152]", "cbnz x9, .Lexit")

    def read(self, operand, bits, register, end):
        if operand.kind == "reg":
            if bits in (8,16):
                self.emit(f"{'ldrb' if bits==8 else 'ldrh'} w{register}, [x19, #{operand.value*8+operand.bit_offset//8}]")
            else:
                prefix = "w" if bits == 32 else "x"
                self.emit(f"ldr {prefix}{register}, [x19, #{operand.value * 8}]")
        elif operand.kind == "imm":
            self.immediate(f"x{register}", operand.value)
        else:
            self.address(operand, end)
            self.emit("mov x0, x19", f"mov x2, #{bits // 8}", "bl _guest_load")
            self.fault_check()
            self.emit(f"mov x{register}, x0")

    def write(self, operand, bits, register, end):
        if operand.kind == "reg":
            if bits in (8,16):
                self.emit(f"{'strb' if bits==8 else 'strh'} w{register}, [x19, #{operand.value*8+operand.bit_offset//8}]")
                return
            if bits == 32:
                self.emit(f"mov w{register}, w{register}")
            self.emit(f"str x{register}, [x19, #{operand.value * 8}]")
        else:
            self.address(operand, end)
            self.emit("mov x0, x19", f"mov x2, x{register}", f"mov x3, #{bits // 8}", "bl _guest_store")
            self.fault_check()

    def stop(self, kind, target=0):
        self.immediate("x9", kind)
        self.emit("str x9, [x19, #168]")
        self.immediate("x9", target)
        if kind == 1:
            self.emit("ldr x10, [x19, #144]", "add x9, x9, x10")
        self.emit("str x9, [x19, #176]", "b .Lexit")


def translate_prefix(code, address, symbol="translated_start", region=False, external_branches=False):
    if not symbol.isascii() or not symbol.isidentifier():
        raise ValueError("Invalid output symbol")
    decoder = Decoder(code, address)
    instructions = []
    while decoder.position < len(code):
        instruction = decoder.next()
        instructions.append(instruction)
        if not region and instruction.kind in ("call", "indirect_call", "ret"):
            break
    if external_branches and not region:
        raise ValueError("External branches require a resumable region")
    if not instructions or (not external_branches and instructions[-1].kind not in ("call", "indirect_call", "ret")):
        raise ValueError("Prefix must end at an explicit call/return boundary")
    boundaries = {instruction.address for instruction in instructions}
    exits = set()
    for instruction in instructions:
        if instruction.kind in ("branch", "jump") and instruction.target not in boundaries:
            if not external_branches or address <= instruction.target < instructions[-1].end:
                raise ValueError("Branch exits translated prefix or targets instruction interior")
            exits.add(instruction.target)
    if external_branches:
        exits.add(instructions[-1].end)
    emitter = Emitter()
    emitter.emit(".text", ".p2align 2", ".globl _translated_start", "_translated_start:",
                 "stp x19, x20, [sp, #-48]!", "stp x21, x22, [sp, #16]", "str x30, [sp, #32]", "mov x19, x0")
    emitter.fault_check()
    if region:
        emitter.emit("ldr x10, [x19, #128]")
        for ins in instructions:
            emitter.immediate("x9", ins.address)
            emitter.emit("cmp x10, x9", f"b.eq .Lpc_{ins.address:x}")
        emitter.emit("mov x9, #7", "str x9, [x19, #152]", "b .Lexit")
    for ins in instructions:
        emitter.emit(f".Lpc_{ins.address:x}:")
        emitter.immediate("x9", ins.address)
        emitter.emit("str x9, [x19, #128]", "ldr x9, [x19, #160]", "cbz x9, .Lfuel", "sub x9, x9, #1", "str x9, [x19, #160]")
        if ins.kind == "push":
            emitter.read(ins.src, 64, 20, ins.end)
            emitter.emit("ldr x21, [x19, #32]", "sub x21, x21, #8", "mov x1, x21", "mov x2, x20", "mov x3, #8", "mov x0, x19", "bl _guest_store")
            emitter.fault_check()
            emitter.emit("str x21, [x19, #32]")
        elif ins.kind == "pop":
            emitter.emit("ldr x1, [x19, #32]", "mov x0, x19", "mov x2, #8", "bl _guest_load")
            emitter.fault_check()
            emitter.emit("mov x20, x0", "ldr x21, [x19, #32]", "add x21, x21, #8", "str x21, [x19, #32]")
            emitter.write(ins.dst, 64, 20, ins.end)
        elif ins.kind=='vector_xor':
            if ins.src.kind=='mem':
                emitter.address(ins.src,ins.end)
                emitter.emit('mov x0, x19',f'mov x2, #{ins.dst.value}',f'mov x3, #{ins.control.value}',f'mov x4, #{ins.bits//8}',
                             'bl _guest_vector_xor')
                emitter.fault_check()
            else:
                emitter.emit(f'ldr q0, [x19, #{208+ins.src.value*32}]',f'ldr q1, [x19, #{208+ins.control.value*32}]','eor v0.16b, v0.16b, v1.16b')
                if ins.bits==256:
                    emitter.emit(f'ldr q2, [x19, #{224+ins.src.value*32}]',f'ldr q3, [x19, #{224+ins.control.value*32}]','eor v2.16b, v2.16b, v3.16b')
                else:
                    emitter.emit('movi v2.16b, #0')
                emitter.emit(f'str q0, [x19, #{208+ins.dst.value*32}]',f'str q2, [x19, #{224+ins.dst.value*32}]')
        elif ins.kind == "vector_move":
            if ins.src.kind=="vector" and ins.dst.kind=="vector":
                emitter.emit(f"ldr q0, [x19, #{208+ins.src.value*32}]")
                if ins.bits==256:
                    emitter.emit(f"ldr q1, [x19, #{224+ins.src.value*32}]")
                else:
                    emitter.emit("movi v1.16b, #0")
                emitter.emit(f"str q0, [x19, #{208+ins.dst.value*32}]",f"str q1, [x19, #{224+ins.dst.value*32}]")
            else:
                load=ins.src.kind=="mem"
                emitter.address(ins.src if load else ins.dst,ins.end)
                reg=ins.dst.value if load else ins.src.value
                emitter.emit("mov x0, x19",f"mov x2, #{reg}",f"mov x3, #{ins.bits//8}",f"mov x4, #{int(ins.aligned)}",
                             "bl _guest_vector_load" if load else "bl _guest_vector_store")
                emitter.fault_check()
        elif ins.kind in ('zero_extend','sign_extend'):
            emitter.read(ins.src,ins.source_bits,20,ins.end)
            if ins.kind=='sign_extend':
                extension={8:'sxtb',16:'sxth',32:'sxtw'}[ins.source_bits]
                emitter.emit(f'{extension} x20, w20')
            emitter.write(ins.dst,ins.bits,20,ins.end)
        elif ins.kind == "mov":
            emitter.read(ins.src, ins.bits, 20, ins.end)
            emitter.write(ins.dst, ins.bits, 20, ins.end)
        elif ins.kind=='movbe':
            emitter.read(ins.src,ins.bits,20,ins.end)
            width='x' if ins.bits==64 else 'w'
            operation='rev16' if ins.bits==16 else 'rev'
            emitter.emit(f'{operation} {width}20, {width}20')
            emitter.write(ins.dst,ins.bits,20,ins.end)
        elif ins.kind == "lea":
            emitter.address(ins.src, ins.end)
            emitter.emit("mov x20, x1")
            emitter.write(ins.dst, ins.bits, 20, ins.end)
        elif ins.kind=='not':
            emitter.read(ins.dst,ins.bits,20,ins.end)
            width='w' if ins.bits==32 else 'x'
            emitter.emit(f'mvn {width}20, {width}20')
            emitter.write(ins.dst,ins.bits,20,ins.end)
        elif ins.kind in ('shlx','shrx','sarx'):
            emitter.read(ins.control,ins.bits,20,ins.end)
            emitter.read(ins.src,ins.bits,21,ins.end)
            width='w' if ins.bits==32 else 'x'
            operation={'shlx':'lsl','shrx':'lsr','sarx':'asr'}[ins.kind]
            emitter.emit(f'and x20, x20, #{ins.bits-1}',f'{operation} {width}22, {width}21, {width}20')
            emitter.write(ins.dst,ins.bits,22,ins.end)
        elif ins.kind=='rorx':
            emitter.read(ins.src,ins.bits,20,ins.end)
            width='x' if ins.bits==64 else 'w'
            emitter.emit(f'ror {width}20, {width}20, #{ins.control.value&(ins.bits-1)}')
            emitter.write(ins.dst,ins.bits,20,ins.end)
        elif ins.kind=='mulx':
            emitter.read(Operand('reg',2),ins.bits,20,ins.end)
            emitter.read(ins.src,ins.bits,21,ins.end)
            if ins.bits==64:
                emitter.emit('mul x22, x20, x21','umulh x20, x20, x21')
            else:
                emitter.emit('umull x22, w20, w21','lsr x20, x22, #32')
            # Read both sources first; the high half wins if destinations alias.
            emitter.write(ins.control,ins.bits,22,ins.end)
            emitter.write(ins.dst,ins.bits,20,ins.end)
        elif ins.kind=='andn':
            emitter.read(ins.control,ins.bits,20,ins.end)
            emitter.read(ins.src,ins.bits,21,ins.end)
            width='w' if ins.bits==32 else 'x'
            emitter.emit(f'bic {width}22, {width}21, {width}20')
            emitter.write(ins.dst,ins.bits,22,ins.end)
            # ANDN defines SF/ZF, clears CF/OF; preserve undefined AF/PF.
            emitter.emit('ldr x9, [x19, #136]','mov x10, #0x8c1','bic x9, x9, x10',
                         'cmp x22, #0','cset x10, eq','orr x9, x9, x10, lsl #6',
                         f'ubfx x10, x22, #{ins.bits-1}, #1','orr x9, x9, x10, lsl #7','str x9, [x19, #136]')
        elif ins.kind=='bextr':
            emitter.read(ins.control,ins.bits,20,ins.end)
            emitter.read(ins.src,ins.bits,21,ins.end)
            zero=f'.Lbextr_zero_{ins.address:x}'
            done=f'.Lbextr_done_{ins.address:x}'
            emitter.emit('and x9, x20, #255','ubfx x10, x20, #8, #8',f'cmp x9, #{ins.bits}',f'b.hs {zero}',f'cbz x10, {zero}',
                         f'mov x11, #{ins.bits}','sub x11, x11, x9','cmp x10, x11','csel x10, x10, x11, lo',
                         'lsr x22, x21, x9','mov x11, #-1','mov x12, #64','sub x12, x12, x10','lsr x11, x11, x12',
                         'and x22, x22, x11',f'b {done}',f'{zero}:','mov x22, #0',f'{done}:')
            emitter.write(ins.dst,ins.bits,22,ins.end)
            # BEXTR defines ZF and clears CF/OF; retain undefined AF/SF/PF.
            emitter.emit('ldr x9, [x19, #136]','mov x10, #0x841','bic x9, x9, x10','cmp x22, #0',
                         'cset x10, eq','orr x9, x9, x10, lsl #6','str x9, [x19, #136]')
        elif ins.kind in ('shl','shr','sar','rol','ror'):
            emitter.read(ins.src,64,20,ins.end)
            emitter.emit(f"and x20, x20, #{ins.bits-1}")
            emitter.read(ins.dst,ins.bits,21,ins.end)
            width='w' if ins.bits==32 else 'x'
            op={'shl':'lsl','shr':'lsr','sar':'asr','ror':'ror','rol':'ror'}[ins.kind]
            count=20
            if ins.kind=='rol':
                emitter.emit('neg x9, x20')
                count=9
            emitter.emit(f'{op} {width}22, {width}21, {width}{count}')
            emitter.write(ins.dst,ins.bits,22,ins.end)
            operation={'shl':0,'shr':1,'sar':2,'rol':3,'ror':4}[ins.kind]
            emitter.emit('mov x0, x19','mov x1, x21','mov x2, x20','mov x3, x22',f'mov x4, #{ins.bits}',f'mov x5, #{operation}',
                         'bl _guest_shift_flags','str x0, [x19, #136]')
            emitter.fault_check()
        elif ins.kind in ("cmp", "sub", "add", "xor", "inc", "test",'and','or'):
            emitter.read(ins.src, ins.bits, 20, ins.end)
            emitter.read(ins.dst, ins.bits, 21, ins.end)
            op = {"cmp": "sub", "sub": "sub", "add": "add", "xor": "eor", "inc": "add", "test":"and",'and':'and','or':'orr'}[ins.kind]
            prefix = "w" if ins.bits == 32 else "x"
            emitter.emit(f"{op} {prefix}22, {prefix}21, {prefix}20")
            if ins.kind not in ("cmp", "test"):
                emitter.write(ins.dst, ins.bits, 22, ins.end)
            operation = {"cmp": 0, "sub": 0, "add": 1, "xor": 2, "inc": 3, "test":2,'and':2,'or':2}[ins.kind]
            emitter.emit("mov x0, x19", "mov x1, x21", "mov x2, x20", "mov x3, x22", f"mov x4, #{ins.bits}", f"mov x5, #{operation}")
            emitter.emit("bl _guest_flags", "str x0, [x19, #136]")
            emitter.fault_check()
        elif ins.kind=='cmov':
            # Legacy CMOV reads its source even when the condition is false.
            emitter.read(ins.src,ins.bits,20,ins.end)
            emitter.read(ins.dst,ins.bits,21,ins.end)
            emitter.emit('mov x0, x19',f'mov x1, #{ins.condition}','bl _guest_condition')
            emitter.fault_check()
            emitter.emit('cmp x0, #0','csel x20, x20, x21, ne')
            emitter.write(ins.dst,ins.bits,20,ins.end)
        elif ins.kind=='setcc':
            emitter.emit('mov x0, x19',f'mov x1, #{ins.condition}','bl _guest_condition')
            emitter.fault_check()
            emitter.emit('mov x20, x0')
            emitter.write(ins.dst,8,20,ins.end)
        elif ins.kind == "branch":
            emitter.emit("mov x0, x19", f"mov x1, #{ins.condition}", "bl _guest_condition", f"cbnz x0, .Lpc_{ins.target:x}")
        elif ins.kind == "jump":
            emitter.emit(f"b .Lpc_{ins.target:x}")
        elif ins.kind == "call":
            emitter.immediate("x9", ins.end)
            emitter.emit("str x9, [x19, #192]")
            emitter.stop(1, ins.target)
        elif ins.kind == "indirect_call":
            emitter.read(ins.src, 64, 20, ins.end)
            emitter.immediate("x9", ins.end)
            emitter.emit("str x9, [x19, #192]")
            emitter.emit("str x20, [x19, #176]", "mov x9, #1", "str x9, [x19, #168]", "b .Lexit")
        elif ins.kind == "ret":
            emitter.stop(3)
    if external_branches:
        emitter.emit(f"b .Lpc_{instructions[-1].end:x}")
        for target in sorted(exits):
            emitter.emit(f".Lpc_{target:x}:")
            emitter.immediate("x9", target)
            emitter.emit("str x9, [x19, #128]")
            emitter.stop(2, target)
    emitter.emit(".Lfuel:", "mov x9, #4", "str x9, [x19, #168]", ".Lexit:", "ldr x30, [sp, #32]", "ldp x21, x22, [sp, #16]", "ldp x19, x20, [sp], #48", "ret")
    assembly = "\n".join(emitter.lines) + "\n"
    assembly = assembly.replace("_translated_start", "_" + symbol).replace(".L", ".L" + symbol + "_")
    return assembly, instructions


def entry_code(data, limit=4096, entry=None):
    if len(data) < 64:
        raise ValueError("Truncated ELF")
    h = struct.unpack_from("<16sHHIQQQIHHHHHH", data)
    if h[0][:7] != b"\x7fELF\x02\x01\x01" or h[1] != 3 or h[2] != 62 or h[9] != 56:
        raise ValueError("Requires x86-64 ET_DYN ELF")
    if h[5] + h[10] * 56 > len(data):
        raise ValueError("ELF program table out of bounds")
    entry = h[4] if entry is None else entry
    for index in range(h[10]):
        kind, flags, offset, address, physical, filesz, memsz, align = struct.unpack_from("<IIQQQQQQ", data, h[5] + index * 56)
        if kind == 1 and flags & 1 and address <= entry < address + filesz:
            if offset + filesz > len(data):
                raise ValueError("ELF code out of bounds")
            start = offset + entry - address
            return entry, data[start:start + min(limit, address + filesz - entry)]
    raise ValueError("No file-backed executable entry")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--entry", type=lambda value: int(value, 0))
    parser.add_argument("--symbol", default="translated_start")
    parser.add_argument("--region-size", type=lambda value: int(value, 0))
    parser.add_argument("--external-branches", action="store_true")
    args = parser.parse_args()
    data = args.input.read_bytes()
    address, code = entry_code(data, entry=args.entry)
    if args.region_size is not None:
        if args.region_size <= 0 or args.region_size > len(code):
            raise ValueError("Invalid region size")
        code = code[:args.region_size]
    assembly, instructions = translate_prefix(code, address, args.symbol, region=args.region_size is not None,
                                              external_branches=args.external_branches)
    args.output.write_text(assembly)
    args.manifest.write_text(json.dumps({"sha256": hashlib.sha256(data).hexdigest(), "entry": address,
        "instructions": [{"address": item.address, "end": item.end, "kind": item.kind, "target": item.target} for item in instructions],
        "stop_before_call": instructions[-1].kind in ("call", "indirect_call")}, indent=2) + "\n")
    print(f"Translated {len(instructions)} startup instructions to ARM64; boundary {instructions[-1].address:#x}")
