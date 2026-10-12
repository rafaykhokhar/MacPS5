import unittest
from startup_aot import Decoder, translate_prefix


class DecoderTests(unittest.TestCase):
    def decode(self, data):
        return Decoder(bytes.fromhex(data), 0x1000).next()

    def test_mulx(self):
        ins=self.decode('c4e2fbf6c0')
        self.assertEqual((ins.kind,ins.bits,ins.dst.value,ins.control.value,ins.src.value),('mulx',64,0,0,0))
        ins=self.decode('c4422bf6c1')
        self.assertEqual((ins.kind,ins.bits,ins.dst.value,ins.control.value,ins.src.value),('mulx',32,8,10,9))
        for raw in ('c4e2fff6c0','c4e2f9f6c0','c4e2faf6c0','c4e3fbf6c0','c4e2fbf6'):
            with self.assertRaises(ValueError): self.decode(raw)

    def test_byte_compare(self):
        ins=self.decode('443a043e')
        self.assertEqual((ins.kind,ins.bits,ins.dst.value,ins.src.kind),('cmp',8,8,'mem'))
        ins=self.decode('38e0')
        self.assertEqual((ins.dst.value,ins.dst.bit_offset,ins.src.value,ins.src.bit_offset),(0,0,0,8))
        ins=self.decode('403ae0')
        self.assertEqual((ins.dst.value,ins.dst.bit_offset,ins.src.value),(4,0,0))

    def test_accumulator_arithmetic(self):
        for raw,kind,bits,value in [('0434','add',8,52),('2c80','sub',8,128),('3cff','cmp',8,255),('0534120000','add',32,0x1234),('482d00000080','sub',64,-0x80000000),('493dffffffff','cmp',64,-1)]:
            instruction=self.decode(raw)
            self.assertEqual((instruction.kind,instruction.bits,instruction.dst.value,instruction.src.value),(kind,bits,0,value))
        for raw in ('0534','483d0000','663d0000'):
            with self.assertRaises(ValueError): self.decode(raw)

    def test_vector_xor(self):
        ins=self.decode('c5f857c0')
        self.assertEqual((ins.kind,ins.bits,ins.dst.value,ins.control.value,ins.src.value),('vector_xor',128,0,0,0))
        ins=self.decode('c4412957c1')
        self.assertEqual((ins.dst.value,ins.control.value,ins.src.value),(8,10,9))
        for raw in ('c5fa57c0','c5fb57c0','c4e27957c0'):
            with self.assertRaises(ValueError): self.decode(raw)

    def test_rorx(self):
        instruction=self.decode('c4c37bf0c31b')
        self.assertEqual((instruction.kind,instruction.bits,instruction.dst.value,instruction.src.value,instruction.control.value),('rorx',32,0,11,27))
        self.assertEqual(self.decode('c4e3fbf001ff').bits,64)
        for raw in ('c4e37ff0c000','c4e373f0c000','c4e378f0c000','c4e37bf0c0'):
            with self.assertRaises(ValueError): self.decode(raw)

    def test_andn(self):
        instruction=self.decode('c4c228f2c0')
        self.assertEqual((instruction.kind,instruction.bits,instruction.dst.value,instruction.control.value,instruction.src.value),('andn',32,0,10,8))
        self.assertEqual(self.decode('c4e2f0f201').bits,64)
        for raw in ('c4e274f2c2','c4e271f2c2','c4e272f2c2','c4e273f2c2'):
            with self.assertRaises(ValueError): self.decode(raw)

    def test_movbe(self):
        for raw,bits in [('0f38f001',32),('660f38f001',16),('480f38f001',64),('66480f38f001',64)]:
            instruction=self.decode(raw)
            self.assertEqual((instruction.kind,instruction.bits,instruction.src.kind),('movbe',bits,'mem'))
        instruction=self.decode('470f38f14c84d0')
        self.assertEqual((instruction.src.value,instruction.dst.base,instruction.dst.index,instruction.dst.scale,instruction.dst.displacement),(9,12,8,4,-48))
        for raw in ('0f38f0c0','0f38f1c0','2e0f38f001','0f38ff01','f20f38f001'):
            with self.assertRaises(ValueError): self.decode(raw)

    def test_extended_registers(self):
        instruction = self.decode("4d 01 c8")
        self.assertEqual((instruction.kind, instruction.bits, instruction.dst.value, instruction.src.value), ("add",64,8,9))

    def test_sib(self):
        instruction = self.decode("48 8b 44 8b f8")
        self.assertEqual((instruction.src.base,instruction.src.index,instruction.src.scale,instruction.src.displacement), (3,1,4,-8))

    def test_rip_relative(self):
        instruction = self.decode("48 8d 05 f9 ff ff ff")
        self.assertTrue(instruction.src.rip)
        self.assertEqual(instruction.end+instruction.src.displacement,0x1000)

    def test_immediate_widths(self):
        instruction = self.decode("48 c7 45 f0 ff ff ff ff")
        self.assertEqual((instruction.bits,instruction.src.value,instruction.dst.displacement),(64,-1,-16))
        instruction = self.decode("41 bf ff ff ff ff")
        self.assertEqual((instruction.bits,instruction.dst.value,instruction.src.value),(32,15,0xffffffff))

    def test_call_targets(self):
        self.assertEqual(self.decode("67 e8 fa ff ff ff").target,0x1000)
        self.assertEqual(self.decode("41 ff d4").src.value,12)

    def test_vex_moves(self):
        instruction=self.decode("c5 f8 28 05 3e f4 28 00")
        self.assertEqual((instruction.kind,instruction.bits,instruction.dst.value,instruction.aligned),('vector_move',128,0,True))
        self.assertTrue(instruction.src.rip)
        instruction=self.decode("c4 41 7c 10 7c 8a 10")
        self.assertEqual((instruction.dst.value,instruction.src.base,instruction.src.index,instruction.src.scale),(15,10,1,4))
        self.assertEqual(instruction.bits,256)
        instruction=self.decode("c5 78 11 c8")
        self.assertEqual((instruction.dst.value,instruction.src.value),(0,9))
        for raw in ('c5 f0 10 c0','c5 f9 28 c0','c4 e2 78 10 c0','48 c5 f8 10 c0',
                    'c5 f8 58 c0','c5','c4 e1','c5 f8 10'):
            with self.subTest(raw=raw),self.assertRaises(ValueError):
                self.decode(raw)

    def test_prefixed_nop_and_byte_memory_immediate(self):
        self.assertEqual(self.decode('66 2e 0f 1f 84 00 00 00 00 00').kind,'nop')
        self.assertEqual(self.decode('66 90').end,0x1002)
        ins=self.decode('43 80 7c 26 01 00')
        self.assertEqual((ins.kind,ins.bits,ins.dst.base,ins.dst.index,ins.src.value),('cmp',8,14,12,0))
        for raw in ('66 89 c0','66 0f 84 00 00 00 00','2e c5 f8 10 c0','66 '*15+'90'):
            with self.subTest(raw=raw),self.assertRaises(ValueError):
                self.decode(raw)

    def test_shifts_and_rotates(self):
        ins=self.decode('48 c1 e8 1d')
        self.assertEqual((ins.kind,ins.bits,ins.dst.value,ins.src.value),('shr',64,0,29))
        ins=self.decode('49 d3 cb')
        self.assertEqual((ins.kind,ins.bits,ins.dst.value,ins.src.kind,ins.src.value),('ror',64,11,'reg',1))
        self.assertEqual(self.decode('d1 e0').src.value,1)
        self.assertEqual(self.decode('48 d3 3a').dst.base,2)
        for raw in ('c1 d0 01','d3 d8','d1 f0','66 d1 e0','d0 e0','c1 e0'):
            with self.subTest(raw=raw),self.assertRaises(ValueError):
                self.decode(raw)

    def test_rejections(self):
        for raw in ("", "48", "48 8b", "48 8b 04", "48 c7 45 f0", "67 8b 00", "f0 01 00", "c6 c8 00", "48 8d c0", "0f 1f c8"):
            with self.subTest(raw=raw), self.assertRaises(ValueError):
                self.decode(raw)
        for raw in ("eb ff c3", "eb 02 c3", "48 89 c0"):
            with self.subTest(raw=raw), self.assertRaises(ValueError):
                translate_prefix(bytes.fromhex(raw),0)

    def test_byte_registers_and_extensions(self):
        legacy=self.decode('88 e0')
        self.assertEqual((legacy.src.value,legacy.src.bit_offset,legacy.dst.value),(0,8,0))
        rex=self.decode('40 88 e0')
        self.assertEqual((rex.src.value,rex.src.bit_offset),(4,0))
        high=self.decode('0f b6 c4')
        self.assertEqual((high.bits,high.source_bits,high.src.value,high.src.bit_offset),(32,8,0,8))
        extended=self.decode('45 0f be c7')
        self.assertEqual((extended.dst.value,extended.src.value,extended.bits,extended.kind),(8,15,32,'sign_extend'))
        self.assertEqual(self.decode('4d 63 c7').source_bits,32)
        self.assertEqual(self.decode('48 c6 c0 80').src.value,128)
        self.assertEqual(self.decode('80 fc 00').dst.bit_offset,8)

    def test_bextr(self):
        ins=self.decode('c4 e2 70 f7 c0')
        self.assertEqual((ins.kind,ins.bits,ins.dst.value,ins.src.value,ins.control.value),('bextr',32,0,0,1))
        ins=self.decode('c4 42 b0 f7 c7')
        self.assertEqual((ins.bits,ins.dst.value,ins.src.value,ins.control.value),(64,8,15,9))
        for raw in ('c4 e2 74 f7 c0','c4 e3 70 f7 c0','c5 f8 f7 c0','c4 e2 70 f7'):
            with self.subTest(raw=raw),self.assertRaises(ValueError):
                self.decode(raw)

    def test_logical_and_bmi_shifts(self):
        ins=self.decode('41 f6 d0')
        self.assertEqual((ins.kind,ins.bits,ins.dst.value),('not',8,8))
        ins=self.decode('41 80 e0 18')
        self.assertEqual((ins.kind,ins.bits,ins.dst.value,ins.src.value),('and',8,8,24))
        ins=self.decode('c4 e2 3b f7 ff')
        self.assertEqual((ins.kind,ins.bits,ins.dst.value,ins.src.value,ins.control.value),('shrx',32,7,7,8))
        self.assertEqual(self.decode('c4 e2 f1 f7 c2').kind,'shlx')
        self.assertEqual(self.decode('c4 e2 72 f7 c2').kind,'sarx')
        self.assertEqual(self.decode('48 25 00 00 00 80').src.value,-2147483648)
        self.assertEqual(self.decode('20 e0').src.bit_offset,8)
        for raw in ('f6 d8','f7 c8 00 00 00 00','80 d0 00','c4 e2 75 f7 c0'):
            with self.subTest(raw=raw),self.assertRaises(ValueError):
                self.decode(raw)

    def test_conditional_data(self):
        ins=self.decode('0f 42 f9')
        self.assertEqual((ins.kind,ins.condition,ins.bits,ins.dst.value,ins.src.value),('cmov',2,32,7,1))
        ins=self.decode('4d 0f 4f c7')
        self.assertEqual((ins.condition,ins.bits,ins.dst.value,ins.src.value),(15,64,8,15))
        ins=self.decode('0f 94 fc')
        self.assertEqual((ins.kind,ins.condition,ins.dst.value,ins.dst.bit_offset),('setcc',4,0,8))
        self.assertEqual(self.decode('40 0f 94 c4').dst.value,4)
        for raw in ('66 0f 44 c0','0f 44','0f 94','f0 0f 94 00'):
            with self.subTest(raw=raw),self.assertRaises(ValueError):
                self.decode(raw)

    def test_backward_branch_is_native(self):
        assembly,_ = translate_prefix(bytes.fromhex("eb fe c3"),0,"loop")
        self.assertIn("b .Lloop_pc_0",assembly)
        self.assertIn("cbz x9, .Lloop_fuel",assembly)

    def test_external_branches(self):
        assembly,_ = translate_prefix(bytes.fromhex("74 2e"),0x50,"external",region=True,external_branches=True)
        self.assertIn("cbnz x0, .Lexternal_pc_80",assembly)
        self.assertIn("b .Lexternal_pc_52",assembly)
        for raw in ("eb ff", "74 ff"):
            with self.subTest(raw=raw), self.assertRaises(ValueError):
                translate_prefix(bytes.fromhex(raw),0x50,region=True,external_branches=True)
        with self.assertRaises(ValueError):
            translate_prefix(bytes.fromhex("c3"),0,external_branches=True)


if __name__ == "__main__":
    unittest.main()
