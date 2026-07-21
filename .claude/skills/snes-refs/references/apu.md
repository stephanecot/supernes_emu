# SNES APU Reference — SPC700 (S-SMP) + S-DSP

Sources: snes.nesdev.org wiki (S-SMP, SPC-700 instruction set, S-DSP registers, DSP envelopes, BRR samples) and fullsnes (problemkaputt.de). All constants transcribed from those pages.

## Clocks
| Item | Value |
|---|---|
| Ceramic resonator | 24.576 MHz (independent of the rest of the SNES; drifts with temperature) |
| S-DSP internal clock | 3.072 MHz |
| S-SMP clock | 2.048 MHz; SPC700 CPU cycle = 2 SMP clocks = 1.024 MHz |
| Sample rate | 1 stereo sample per 768 resonator cycles = 32000 Hz nominal = 32 CPU cycles/sample |
| ARAM arbitration | 64 KiB ARAM time-shared: per CPU cycle 1 S-SMP access + 2 S-DSP accesses |

## SPC700 register set
| Reg | Size | Notes |
|---|---|---|
| A | 8 | accumulator |
| X, Y | 8 | index registers |
| YA | 16 | pair: Y = MSB, A = LSB (16-bit ops, MUL/DIV) |
| SP | 8 | stack at $0100+SP, push decrements |
| PC | 16 | reset vector at $FFFE/$FFFF (IPL ROM contains $FFC0) |
| PSW | 8 | flags N V P B H I Z C |

PSW flags (bit 7..0 = N V P B H I Z C):
| Bit | Flag | Meaning |
|---|---|---|
| 7 | N | negative (bit 7 of result) |
| 6 | V | signed overflow |
| 5 | P | direct-page select: 0 = dp at $00xx, 1 = dp at $01xx |
| 4 | B | break flag (set after BRK) |
| 3 | H | half-carry (carry bit3→bit4; for ADDW/SUBW carry bit11→bit12) |
| 2 | I | interrupt enable — no interrupt sources on SNES APU, no effect |
| 1 | Z | zero |
| 0 | C | carry |

Reset/power-on state after IPL runs: A=X=Y=0, SP=$EF, PSW=$02 at entry to uploaded code.

## Addressing modes
| Syntax | Meaning |
|---|---|
| #i | 8-bit immediate |
| d | direct page: $0000+d (P=0) or $0100+d (P=1) |
| d+X, d+Y | dp indexed; wraps within the direct page: addr = (d+X) & $FF (+P*$100) |
| !a | 16-bit absolute |
| !a+X, !a+Y | absolute indexed |
| (X), (Y) | dp pointed by X / Y (P*$100 + X) |
| (X)+ | (X) with X post-increment |
| [d+X] | indexed indirect: word at dp (d+X) → address |
| [d]+Y | indirect indexed: word at dp d, +Y after lookup |
| d.b | bit b of a dp address |
| m.b | 13-bit absolute address (operand bits 0-12) + bit number (operand bits 13-15) |
| r | 8-bit signed relative branch offset |
| u | PCALL: target $FF00+u |
| n | TCALL n: target = word at $FFDE-2*n |

## Full opcode table (256 opcodes)
B = bytes, C = cycles at 1.024 MHz (1 cycle = 2 SMP clocks). "x/y" = not-taken/taken.
Notes: taken conditional branches add 2 cycles. Most store opcodes perform a dummy read of the destination first (can trigger read-sensitive $FD-$FF); exceptions: $AF MOV (X)+,A and $FA MOV d,d (no read), $DA MOVW d,YA (dummy-reads LSB only). MOVW d,YA is 5 cycles per fullsnes/ares (nesdev wiki lists 4 — discrepancy, 5 matches hardware-verified emulators). SLEEP/STOP halt the CPU forever (no wake source). DIV: A=YA/X, Y=YA%X, V=bit8 of quotient, result valid only if quotient ≤ 511, N/Z from A. MUL: YA=Y*A, N/Z from Y only. TSET1/TCLR1 set N/Z from CMP A,[!a] (equality test), then [!a] |= A / &= ~A.

| Op | Instr | B | C | Op | Instr | B | C | Op | Instr | B | C | Op | Instr | B | C |
|----|-------|---|---|----|-------|---|---|----|-------|---|---|----|-------|---|---|
| $00 | NOP | 1 | 2 | $40 | SETP | 1 | 2 | $80 | SETC | 1 | 2 | $C0 | DI | 1 | 3 |
| $01 | TCALL 0 | 1 | 8 | $41 | TCALL 4 | 1 | 8 | $81 | TCALL 8 | 1 | 8 | $C1 | TCALL 12 | 1 | 8 |
| $02 | SET1 d.0 | 2 | 4 | $42 | SET1 d.2 | 2 | 4 | $82 | SET1 d.4 | 2 | 4 | $C2 | SET1 d.6 | 2 | 4 |
| $03 | BBS d.0,r | 3 | 5/7 | $43 | BBS d.2,r | 3 | 5/7 | $83 | BBS d.4,r | 3 | 5/7 | $C3 | BBS d.6,r | 3 | 5/7 |
| $04 | OR A,d | 2 | 3 | $44 | EOR A,d | 2 | 3 | $84 | ADC A,d | 2 | 3 | $C4 | MOV d,A | 2 | 4 |
| $05 | OR A,!a | 3 | 4 | $45 | EOR A,!a | 3 | 4 | $85 | ADC A,!a | 3 | 4 | $C5 | MOV !a,A | 3 | 5 |
| $06 | OR A,(X) | 1 | 3 | $46 | EOR A,(X) | 1 | 3 | $86 | ADC A,(X) | 1 | 3 | $C6 | MOV (X),A | 1 | 4 |
| $07 | OR A,[d+X] | 2 | 6 | $47 | EOR A,[d+X] | 2 | 6 | $87 | ADC A,[d+X] | 2 | 6 | $C7 | MOV [d+X],A | 2 | 7 |
| $08 | OR A,#i | 2 | 2 | $48 | EOR A,#i | 2 | 2 | $88 | ADC A,#i | 2 | 2 | $C8 | CMP X,#i | 2 | 2 |
| $09 | OR d,d | 3 | 6 | $49 | EOR d,d | 3 | 6 | $89 | ADC d,d | 3 | 6 | $C9 | MOV !a,X | 3 | 5 |
| $0A | OR1 C,m.b | 3 | 5 | $4A | AND1 C,m.b | 3 | 4 | $8A | EOR1 C,m.b | 3 | 5 | $CA | MOV1 m.b,C | 3 | 6 |
| $0B | ASL d | 2 | 4 | $4B | LSR d | 2 | 4 | $8B | DEC d | 2 | 4 | $CB | MOV d,Y | 2 | 4 |
| $0C | ASL !a | 3 | 5 | $4C | LSR !a | 3 | 5 | $8C | DEC !a | 3 | 5 | $CC | MOV !a,Y | 3 | 5 |
| $0D | PUSH PSW | 1 | 4 | $4D | PUSH X | 1 | 4 | $8D | MOV Y,#i | 2 | 2 | $CD | MOV X,#i | 2 | 2 |
| $0E | TSET1 !a | 3 | 6 | $4E | TCLR1 !a | 3 | 6 | $8E | POP PSW | 1 | 4 | $CE | POP X | 1 | 4 |
| $0F | BRK | 1 | 8 | $4F | PCALL u | 2 | 6 | $8F | MOV d,#i | 3 | 5 | $CF | MUL YA | 1 | 9 |
| $10 | BPL r | 2 | 2/4 | $50 | BVC r | 2 | 2/4 | $90 | BCC r | 2 | 2/4 | $D0 | BNE r | 2 | 2/4 |
| $11 | TCALL 1 | 1 | 8 | $51 | TCALL 5 | 1 | 8 | $91 | TCALL 9 | 1 | 8 | $D1 | TCALL 13 | 1 | 8 |
| $12 | CLR1 d.0 | 2 | 4 | $52 | CLR1 d.2 | 2 | 4 | $92 | CLR1 d.4 | 2 | 4 | $D2 | CLR1 d.6 | 2 | 4 |
| $13 | BBC d.0,r | 3 | 5/7 | $53 | BBC d.2,r | 3 | 5/7 | $93 | BBC d.4,r | 3 | 5/7 | $D3 | BBC d.6,r | 3 | 5/7 |
| $14 | OR A,d+X | 2 | 4 | $54 | EOR A,d+X | 2 | 4 | $94 | ADC A,d+X | 2 | 4 | $D4 | MOV d+X,A | 2 | 5 |
| $15 | OR A,!a+X | 3 | 5 | $55 | EOR A,!a+X | 3 | 5 | $95 | ADC A,!a+X | 3 | 5 | $D5 | MOV !a+X,A | 3 | 6 |
| $16 | OR A,!a+Y | 3 | 5 | $56 | EOR A,!a+Y | 3 | 5 | $96 | ADC A,!a+Y | 3 | 5 | $D6 | MOV !a+Y,A | 3 | 6 |
| $17 | OR A,[d]+Y | 2 | 6 | $57 | EOR A,[d]+Y | 2 | 6 | $97 | ADC A,[d]+Y | 2 | 6 | $D7 | MOV [d]+Y,A | 2 | 7 |
| $18 | OR d,#i | 3 | 5 | $58 | EOR d,#i | 3 | 5 | $98 | ADC d,#i | 3 | 5 | $D8 | MOV d,X | 2 | 4 |
| $19 | OR (X),(Y) | 1 | 5 | $59 | EOR (X),(Y) | 1 | 5 | $99 | ADC (X),(Y) | 1 | 5 | $D9 | MOV d+Y,X | 2 | 5 |
| $1A | DECW d | 2 | 6 | $5A | CMPW YA,d | 2 | 4 | $9A | SUBW YA,d | 2 | 5 | $DA | MOVW d,YA | 2 | 5 |
| $1B | ASL d+X | 2 | 5 | $5B | LSR d+X | 2 | 5 | $9B | DEC d+X | 2 | 5 | $DB | MOV d+X,Y | 2 | 5 |
| $1C | ASL A | 1 | 2 | $5C | LSR A | 1 | 2 | $9C | DEC A | 1 | 2 | $DC | DEC Y | 1 | 2 |
| $1D | DEC X | 1 | 2 | $5D | MOV X,A | 1 | 2 | $9D | MOV X,SP | 1 | 2 | $DD | MOV A,Y | 1 | 2 |
| $1E | CMP X,!a | 3 | 4 | $5E | CMP Y,!a | 3 | 4 | $9E | DIV YA,X | 1 | 12 | $DE | CBNE d+X,r | 3 | 6/8 |
| $1F | JMP [!a+X] | 3 | 6 | $5F | JMP !a | 3 | 3 | $9F | XCN A | 1 | 5 | $DF | DAA A | 1 | 3 |
| $20 | CLRP | 1 | 2 | $60 | CLRC | 1 | 2 | $A0 | EI | 1 | 3 | $E0 | CLRV | 1 | 2 |
| $21 | TCALL 2 | 1 | 8 | $61 | TCALL 6 | 1 | 8 | $A1 | TCALL 10 | 1 | 8 | $E1 | TCALL 14 | 1 | 8 |
| $22 | SET1 d.1 | 2 | 4 | $62 | SET1 d.3 | 2 | 4 | $A2 | SET1 d.5 | 2 | 4 | $E2 | SET1 d.7 | 2 | 4 |
| $23 | BBS d.1,r | 3 | 5/7 | $63 | BBS d.3,r | 3 | 5/7 | $A3 | BBS d.5,r | 3 | 5/7 | $E3 | BBS d.7,r | 3 | 5/7 |
| $24 | AND A,d | 2 | 3 | $64 | CMP A,d | 2 | 3 | $A4 | SBC A,d | 2 | 3 | $E4 | MOV A,d | 2 | 3 |
| $25 | AND A,!a | 3 | 4 | $65 | CMP A,!a | 3 | 4 | $A5 | SBC A,!a | 3 | 4 | $E5 | MOV A,!a | 3 | 4 |
| $26 | AND A,(X) | 1 | 3 | $66 | CMP A,(X) | 1 | 3 | $A6 | SBC A,(X) | 1 | 3 | $E6 | MOV A,(X) | 1 | 3 |
| $27 | AND A,[d+X] | 2 | 6 | $67 | CMP A,[d+X] | 2 | 6 | $A7 | SBC A,[d+X] | 2 | 6 | $E7 | MOV A,[d+X] | 2 | 6 |
| $28 | AND A,#i | 2 | 2 | $68 | CMP A,#i | 2 | 2 | $A8 | SBC A,#i | 2 | 2 | $E8 | MOV A,#i | 2 | 2 |
| $29 | AND d,d | 3 | 6 | $69 | CMP d,d | 3 | 6 | $A9 | SBC d,d | 3 | 6 | $E9 | MOV X,!a | 3 | 4 |
| $2A | OR1 C,/m.b | 3 | 5 | $6A | AND1 C,/m.b | 3 | 4 | $AA | MOV1 C,m.b | 3 | 4 | $EA | NOT1 m.b | 3 | 5 |
| $2B | ROL d | 2 | 4 | $6B | ROR d | 2 | 4 | $AB | INC d | 2 | 4 | $EB | MOV Y,d | 2 | 3 |
| $2C | ROL !a | 3 | 5 | $6C | ROR !a | 3 | 5 | $AC | INC !a | 3 | 5 | $EC | MOV Y,!a | 3 | 4 |
| $2D | PUSH A | 1 | 4 | $6D | PUSH Y | 1 | 4 | $AD | CMP Y,#i | 2 | 2 | $ED | NOTC | 1 | 3 |
| $2E | CBNE d,r | 3 | 5/7 | $6E | DBNZ d,r | 3 | 5/7 | $AE | POP A | 1 | 4 | $EE | POP Y | 1 | 4 |
| $2F | BRA r | 2 | 4 | $6F | RET | 1 | 5 | $AF | MOV (X)+,A | 1 | 4 | $EF | SLEEP | 1 | 3 |
| $30 | BMI r | 2 | 2/4 | $70 | BVS r | 2 | 2/4 | $B0 | BCS r | 2 | 2/4 | $F0 | BEQ r | 2 | 2/4 |
| $31 | TCALL 3 | 1 | 8 | $71 | TCALL 7 | 1 | 8 | $B1 | TCALL 11 | 1 | 8 | $F1 | TCALL 15 | 1 | 8 |
| $32 | CLR1 d.1 | 2 | 4 | $72 | CLR1 d.3 | 2 | 4 | $B2 | CLR1 d.5 | 2 | 4 | $F2 | CLR1 d.7 | 2 | 4 |
| $33 | BBC d.1,r | 3 | 5/7 | $73 | BBC d.3,r | 3 | 5/7 | $B3 | BBC d.5,r | 3 | 5/7 | $F3 | BBC d.7,r | 3 | 5/7 |
| $34 | AND A,d+X | 2 | 4 | $74 | CMP A,d+X | 2 | 4 | $B4 | SBC A,d+X | 2 | 4 | $F4 | MOV A,d+X | 2 | 4 |
| $35 | AND A,!a+X | 3 | 5 | $75 | CMP A,!a+X | 3 | 5 | $B5 | SBC A,!a+X | 3 | 5 | $F5 | MOV A,!a+X | 3 | 5 |
| $36 | AND A,!a+Y | 3 | 5 | $76 | CMP A,!a+Y | 3 | 5 | $B6 | SBC A,!a+Y | 3 | 5 | $F6 | MOV A,!a+Y | 3 | 5 |
| $37 | AND A,[d]+Y | 2 | 6 | $77 | CMP A,[d]+Y | 2 | 6 | $B7 | SBC A,[d]+Y | 2 | 6 | $F7 | MOV A,[d]+Y | 2 | 6 |
| $38 | AND d,#i | 3 | 5 | $78 | CMP d,#i | 3 | 5 | $B8 | SBC d,#i | 3 | 5 | $F8 | MOV X,d | 2 | 3 |
| $39 | AND (X),(Y) | 1 | 5 | $79 | CMP (X),(Y) | 1 | 5 | $B9 | SBC (X),(Y) | 1 | 5 | $F9 | MOV X,d+Y | 2 | 4 |
| $3A | INCW d | 2 | 6 | $7A | ADDW YA,d | 2 | 5 | $BA | MOVW YA,d | 2 | 5 | $FA | MOV d,d | 3 | 5 |
| $3B | ROL d+X | 2 | 5 | $7B | ROR d+X | 2 | 5 | $BB | INC d+X | 2 | 5 | $FB | MOV Y,d+X | 2 | 4 |
| $3C | ROL A | 1 | 2 | $7C | ROR A | 1 | 2 | $BC | INC A | 1 | 2 | $FC | INC Y | 1 | 2 |
| $3D | INC X | 1 | 2 | $7D | MOV A,X | 1 | 2 | $BD | MOV SP,X | 1 | 2 | $FD | MOV Y,A | 1 | 2 |
| $3E | CMP X,d | 2 | 3 | $7E | CMP Y,d | 2 | 3 | $BE | DAS A | 1 | 3 | $FE | DBNZ Y,r | 2 | 4/6 |
| $3F | CALL !a | 3 | 8 | $7F | RETI | 1 | 6 | $BF | MOV A,(X)+ | 1 | 4 | $FF | STOP | 1 | 2 |

## Memory map (64 KB ARAM)
| Range | Contents |
|---|---|
| $0000-$00EF | zero page RAM (direct page when P=0) |
| $00F0-$00FF | I/O port overlay (registers below) |
| $0100-$01FF | stack page RAM (direct page when P=1) |
| $0200-$FFBF | RAM |
| $FFC0-$FFFF | 64-byte IPL ROM (when CONTROL bit7=1) or RAM; writes always go to the underlying RAM |

## I/O registers $F0-$FF
| Addr | Name | R/W | Function |
|---|---|---|---|
| $F0 | TEST | W | undocumented test register. Power-on = $0A. bit0: halt timers (1=stopped); bit1: RAM write enable (0=read-only); bit2: disable RAM reads / crash; bit3: timer enable (0=timers stopped, 1=normal); bits4-5: RAM waitstates (0..3 = 0/1/4/9 extra cycles); bits6-7: I/O+ROM waitstates (0..3 = 0/1/4/9). Responds to writes only when PSW.P=0. Never written by normal software |
| $F1 | CONTROL | W | bit0-2: enable timer 0/1/2 (0→1 transition resets that timer's internal counter and TnOUT to 0); bit4: reset CPUIO0/1 input latches ($F4/$F5) to $00; bit5: reset CPUIO2/3 latches ($F6/$F7) to $00; bit7: IPL ROM enable at $FFC0-$FFFF (1=ROM). Reset value $B0. Port clears affect only the SPC side inputs; S-CPU-side APUIO outputs unchanged |
| $F2 | DSPADDR | R/W | DSP register address. Writing with bit7 set selects (addr & $7F) but makes DSPDATA **read-only** (writes to $F3 dropped). Bit7 always reads back 0; $80-$FF are read-only mirrors of $00-$7F |
| $F3 | DSPDATA | R/W | read/write DSP register selected by $F2 (write ignored if $F2 bit7 was set) |
| $F4-$F7 | CPUIO0-3 | R/W | 4 comm ports = 8 one-way latches. Read: last byte written by S-CPU to $2140-$2143. Write: byte readable by S-CPU at $2140-$2143. Reset = $00. Simultaneous write+read on the same port yields OR of old|new on the reader's side |
| $F8, $F9 | AUXIO4/5 | R/W | external port pins, unconnected on SNES → behave as normal RAM-like storage |
| $FA | T0DIV | W | timer 0 target (stage-1 clock 8000 Hz). $01-$FF = divide by 1-255, $00 = divide by 256 |
| $FB | T1DIV | W | timer 1 target (8000 Hz), same encoding |
| $FC | T2DIV | W | timer 2 target (64000 Hz), same encoding |
| $FD | T0OUT | R | 4-bit up-counter (bits 4-7 = 0). **Reading resets it to 0.** |
| $FE | T1OUT | R | same, timer 1 |
| $FF | T2OUT | R | same, timer 2 |

Write-only registers read back $00 ($F0, $F1, $FA-$FC).

### Timer semantics
- Stage 1: fixed prescaler from the 1.024 MHz CPU clock — timers 0,1 tick at 8 kHz (÷128), timer 2 at 64 kHz (÷16). Ticks are synchronous with the DSP sample loop (at slots T1 and T17 of the 32-cycle loop; the two 8 kHz timers tick every 4th sample).
- Stage 2: each stage-1 tick increments an internal 8-bit counter; when it reaches the target ($FA-$FC; $00 means 256), the counter resets to 0 and the 4-bit TnOUT ($FD-$FF) increments (wraps 15→0).
- CONTROL bit n 0→1: resets internal counter and TnOUT to 0. Reading TnOUT returns the count then clears it.
- TEST ($F0) bits 0/3 can halt all timers.

## IPL boot ROM (64 bytes at $FFC0)
Exact dump (fullsnes "Boot ROM Disassembly", bytes assembled from disassembly):
```
FFC0: CD EF BD E8 00 C6 1D D0 FC 8F AA F4 8F BB F5 78
FFD0: CC F4 D0 FB 2F 19 EB F4 D0 FC 7E F4 D0 0B E4 F5
FFE0: CB F4 D7 00 FC D0 F3 AB 01 10 EF 7E F4 10 EB BA
FFF0: F6 DA 00 BA F4 C4 F4 DD 5D D0 DB 1F 00 00 C0 FF
```
$FFFE/$FFFF = reset vector = $FFC0.

IPL program flow:
1. SP=$EF; zero-fill $0001-$00EF (loop `mov [x],a / dec x`; also writes $00 to $00).
2. Ready signal: write $AA to $F4 (port0), $BB to $F5 (port1).
3. Wait until port0 reads $CC.
4. `main`: copy word from ports 2/3 ($F6/$F7) to $0000/$0001 (dest or entry address); read port0 (kick) into A, echo A back to port0 (ack); command = port1 value read into Y.
5. If command ≠ 0 → transfer: wait until port0 reads 0; loop: wait port0 == Y (index), read data from port1, ack by writing Y to port0, store via `mov [[00]+y],a`, Y++ (inc $01 on wrap). If port0 > Y → ack and go back to step 4 (new block/entry). If port0 < Y keep waiting.
6. If command = 0 → `jmp [$0000+X]` with A=0, X=0, Y=0, SP=$EF, PSW=$02.

S-CPU side upload protocol ($2140-$2143 = APU ports 0-3):
1. Wait word[$2140] == $BBAA.
2. Per block: write dest addr to $2142/$2143, non-zero to $2141, kick to $2140 (first kick = $CC); wait $2140 == kick.
3. First byte: write data to $2141, $00 to $2140; wait $2140 == 0. Each next byte: data to $2141, ++counter to $2140, wait $2140 == counter.
4. Next block or start: kick = (last index + 2) & $FF, and if starting another transfer it must be non-zero (bump if 0, and > last index+1). To execute: write entry point to $2142/$2143, $00 to $2141, kick to $2140; wait for ack. Execution begins at entry.
- ~520 master clocks per byte (~650 bytes per 60 Hz frame). The last-byte ack window is short: disable NMI/IRQ during upload.
- Many games jump back to $FFC0 to re-enter the IPL loader (requires CONTROL bit7=1).

## S-DSP register map (accessed via $F2/$F3; 128-byte space $00-$7F)
### Per-voice, voice x = 0-7, address $x0-$x9
| Addr | Name | Bits | Function |
|---|---|---|---|
| $x0 | VxVOLL | signed 8 | left volume, out = sample*vol/128 (negative inverts phase; -128 safe here) |
| $x1 | VxVOLR | signed 8 | right volume |
| $x2 | VxPITCHL | low 8 | pitch low byte |
| $x3 | VxPITCHH | --HHHHHH | pitch bits 8-13 (14-bit total, $1000 = 32000 Hz, max $3FFF; rate = P*32000/$1000) |
| $x4 | VxSRCN | 8 | sample directory entry index (takes effect at next key-on/loop fetch) |
| $x5 | VxADSR1 | EDDDAAAA | E=1: ADSR mode; D = decay rate, A = attack rate |
| $x6 | VxADSR2 | LLLRRRRR | LLL = sustain level, RRRRR = sustain rate |
| $x7 | VxGAIN | 0VVVVVVV / 1MMRRRRR | (ADSR1.E=0) direct gain env=V*16, or mode MM + rate RRRRR |
| $x8 | VxENVX | R, 0EEEEEEE | current envelope, upper 7 bits of internal 11-bit value; updated once/sample |
| $x9 | VxOUTX | R, signed 8 | upper 8 bits of the 15-bit voice sample after envelope, before VxVOL; updated once/sample |
| $xA, $xB, $xE (x=0-7), $1D | — | — | unused, act as plain register RAM |

### Global
| Addr | Name | Function |
|---|---|---|
| $0C | MVOLL | main volume L, signed (avoid -128: multiply overflow) |
| $1C | MVOLR | main volume R, signed |
| $2C | EVOLL | echo volume L, signed |
| $3C | EVOLR | echo volume R, signed |
| $4C | KON | key-on bits 7..0 per voice: env=0, state=Attack, restart BRR from DIR/SRCN start address |
| $5C | KOFF | key-off bits: state=Release (env -8/sample) |
| $6C | FLG | bit7 soft reset (all voices → Release, env=0; echo still runs); bit6 mute amplifier (internal processing continues); bit5 echo-write disable (reads and buffer-position advance continue); bits0-4 noise frequency (rate-table index). Internal value at reset = $E0, but reading before first write returns garbage |
| $7C | ENDX | per-voice flag, set at the START of decoding a BRR block whose end bit is set (also during Release). Cleared for a voice by key-on; ANY write to $7C clears ALL bits |
| $0D | EFB | echo feedback volume, signed |
| $2D | PMON | bits 1-7: pitch-modulate voice x by OUTX of voice x-1 (bit0 unused) |
| $3D | NON | per-voice: replace BRR output with noise (single shared generator; BRR keeps decoding — an End+Mute block still releases the voice) |
| $4D | EON | per-voice: mix voice into echo input |
| $5D | DIR | sample directory page: table at DIR*$100 |
| $6D | ESA | echo buffer page: buffer at ESA*$100 |
| $7D | EDL | bits 0-3: echo delay; buffer = EDL*2048 bytes = EDL*512 samples (16 ms steps, max 240 ms); EDL=0 → 4-byte buffer |
| $0F,$1F,...,$7F | FIR0-7 | signed 8-tap FIR coefficients (FIR7 at $7F is the newest tap) |

Register-access cautions: DSP polls registers at fixed slots inside the 32-cycle sample loop, so most writes take effect at the next poll; KON/KOFF are polled every 2nd sample; reads of $F3 return the register RAM, which may lag internal state (ENVX/OUTX/ENDX are written back by the DSP itself).

## Sample directory
At DIR*$100 + SRCN*4, four bytes per entry (up to 256 entries):
| Offset | Content |
|---|---|
| 0-1 | BRR start address (little-endian), used at key-on |
| 2-3 | BRR loop address, used when a block with end flag set finishes |

## BRR block format
9 bytes = header + 16 samples (2 per byte, high nibble first).
Header `SSSS FFLE`: S = shift 0-12 (13-15 reserved), F = filter 0-3, L = loop, E = end.
End/loop codes: 0 or 2 = continue to next block; 1 = end+mute (jump to loop address, set ENDX, force Release with env=0); 3 = end+loop (jump to loop address, set ENDX, keep playing).

Nibble decode (nibble = signed -8..+7):
- `sample = (nibble << shift) >> 1` (arithmetic). Shift 13-15 behaves as shift=12 with nibble replaced by `nibble >> 3` (arith.), i.e. result $0000 or $F800.

Filter equations (old = previous output, older = one before; exact integer forms):
| F | Formula (exact) | ≈ coefficients |
|---|---|---|
| 0 | new = sample | direct |
| 1 | new = sample + old + ((-old) >> 4) | old*15/16 |
| 2 | new = sample + 2*old + ((-3*old) >> 5) - older + (older >> 4) | old*61/32 - older*15/16 |
| 3 | new = sample + 2*old + ((-13*old) >> 6) - older + ((3*older) >> 4) | old*115/64 - older*13/16 |

(all shifts arithmetic). Clamping after each sample:
1. Clamp to signed 16-bit: >$7FFF → $7FFF; <-$8000 → -$8000.
2. Then clip to 15 bits by sign-dropping bit 15: +$4000..+$7FFF → -$4000..-1, -$8000..-$4001 → 0..-$3FFF (i.e. keep low 15 bits, sign-extend bit 14).
The 15-bit value feeds the Gaussian filter and becomes `old` (previous `old` → `older`).

## Pitch counter (per voice, stepped at 32000 Hz)
```
step = VxPITCH                         ; 0..$3FFF
if PMON bit x set and x>0:
    factor = (OUTX[x-1] >> 4) + $400   ; OUTX = 15-bit -$4000..+$3FFF → factor 0..$7FF (0.0..2.0)
    step = (step * factor) >> 10       ; result clamped to $3FFF max
counter = counter + step               ; 16-bit; carry → advance to next BRR block
```
- counter bits 15-12 = current sample index within the interpolation window/BRR block; bits 11-4 = Gaussian interpolation index i.
- Rates above $1000 skip source samples (output stays 32 kHz).

## Gaussian interpolation
4 most recent decoded 15-bit samples (new, old, older, oldest), i = counter bits 4-11 ($00-$FF):
```
out =        (gauss[$0FF-i] * oldest) >> 10    ; no 16-bit overflow handling
out = out + ((gauss[$1FF-i] * older ) >> 10)   ; can overflow (i=$00..$1F) — wraps (hardware bug)
out = out + ((gauss[$100+i] * old   ) >> 10)   ; can overflow (i=$20..$FF) — saturated to ±
out = out + ((gauss[$000+i] * new   ) >> 10)   ; with 16-bit saturation
out = out >> 1                                 ; 15-bit result
```
The four taps sum to $7FF..$801 (not exactly $800): three max-negative inputs in a row can overflow to +$3FF8 (audible pop). After interpolation: `out = out * ENVX_11bit / $800` (envelope applied), giving OUTX.

Full 512-entry Gauss table (hex, 16 per row, from fullsnes — verified monotonic $000→$519):
```
  $000: 000 000 000 000 000 000 000 000 000 000 000 000 000 000 000 000
  $010: 001 001 001 001 001 001 001 001 001 001 001 002 002 002 002 002
  $020: 002 002 003 003 003 003 003 004 004 004 004 004 005 005 005 005
  $030: 006 006 006 006 007 007 007 008 008 008 009 009 009 00A 00A 00A
  $040: 00B 00B 00B 00C 00C 00D 00D 00E 00E 00F 00F 00F 010 010 011 011
  $050: 012 013 013 014 014 015 015 016 017 017 018 018 019 01A 01B 01B
  $060: 01C 01D 01D 01E 01F 020 020 021 022 023 024 024 025 026 027 028
  $070: 029 02A 02B 02C 02D 02E 02F 030 031 032 033 034 035 036 037 038
  $080: 03A 03B 03C 03D 03E 040 041 042 043 045 046 047 049 04A 04C 04D
  $090: 04E 050 051 053 054 056 057 059 05A 05C 05E 05F 061 063 064 066
  $0A0: 068 06A 06B 06D 06F 071 073 075 076 078 07A 07C 07E 080 082 084
  $0B0: 086 089 08B 08D 08F 091 093 096 098 09A 09C 09F 0A1 0A3 0A6 0A8
  $0C0: 0AB 0AD 0AF 0B2 0B4 0B7 0BA 0BC 0BF 0C1 0C4 0C7 0C9 0CC 0CF 0D2
  $0D0: 0D4 0D7 0DA 0DD 0E0 0E3 0E6 0E9 0EC 0EF 0F2 0F5 0F8 0FB 0FE 101
  $0E0: 104 107 10B 10E 111 114 118 11B 11E 122 125 129 12C 130 133 137
  $0F0: 13A 13E 141 145 148 14C 150 153 157 15B 15F 162 166 16A 16E 172
  $100: 176 17A 17D 181 185 189 18D 191 195 19A 19E 1A2 1A6 1AA 1AE 1B2
  $110: 1B7 1BB 1BF 1C3 1C8 1CC 1D0 1D5 1D9 1DD 1E2 1E6 1EB 1EF 1F3 1F8
  $120: 1FC 201 205 20A 20F 213 218 21C 221 226 22A 22F 233 238 23D 241
  $130: 246 24B 250 254 259 25E 263 267 26C 271 276 27B 280 284 289 28E
  $140: 293 298 29D 2A2 2A6 2AB 2B0 2B5 2BA 2BF 2C4 2C9 2CE 2D3 2D8 2DC
  $150: 2E1 2E6 2EB 2F0 2F5 2FA 2FF 304 309 30E 313 318 31D 322 326 32B
  $160: 330 335 33A 33F 344 349 34E 353 357 35C 361 366 36B 370 374 379
  $170: 37E 383 388 38C 391 396 39B 39F 3A4 3A9 3AD 3B2 3B7 3BB 3C0 3C5
  $180: 3C9 3CE 3D2 3D7 3DC 3E0 3E5 3E9 3ED 3F2 3F6 3FB 3FF 403 408 40C
  $190: 410 415 419 41D 421 425 42A 42E 432 436 43A 43E 442 446 44A 44E
  $1A0: 452 455 459 45D 461 465 468 46C 470 473 477 47A 47E 481 485 488
  $1B0: 48C 48F 492 496 499 49C 49F 4A2 4A6 4A9 4AC 4AF 4B2 4B5 4B7 4BA
  $1C0: 4BD 4C0 4C3 4C5 4C8 4CB 4CD 4D0 4D2 4D5 4D7 4D9 4DC 4DE 4E0 4E3
  $1D0: 4E5 4E7 4E9 4EB 4ED 4EF 4F1 4F3 4F5 4F6 4F8 4FA 4FB 4FD 4FF 500
  $1E0: 502 503 504 506 507 508 50A 50B 50C 50D 50E 50F 510 511 511 512
  $1F0: 513 514 514 515 516 516 517 517 517 518 518 518 518 518 519 519
```

## Envelopes (ADSR / GAIN)
Internal envelope: 11-bit, 0..$7FF, clamped (not wrapped). ENVX = upper 7 bits.

ADSR (ADSR1 bit7 = 1):
| Phase | Rate index | Step |
|---|---|---|
| Attack | A*2+1 | +32 per period; if A=$F: +1024 at rate 31 (every sample) |
| Decay | D*2+16 | exponential: env -= 1; env -= env >> 8 |
| Sustain | SR (RRRRR) | exponential, same formula; SR=0 → hold forever |
| Release (key-off) | fixed | -8 every sample (also: -$800 instantly on BRR End+Mute block) |
- Attack→Decay when env ≥ $7E0 (clip to $7FF).
- Decay→Sustain when env ≤ (SL+1)*$100 (SL = ADSR2 bits 5-7).
- GAIN modes (ADSR1 bit7 = 0): GAIN bit7=0 → direct: env = V*16, no stepping. GAIN bit7=1, mode MM: 00 linear decrease -32; 01 exponential decrease (env-=1; env-=env>>8); 10 linear increase +32; 11 bent increase (+32 while env < $600, else +8), all at rate RRRRR.
- Hardware still tracks attack/decay/sustain phase in GAIN mode; the decay→sustain boundary compare erroneously uses GAIN bits 5-7 instead of ADSR2 bits 5-7.

### Rate/period table (32 entries; value = samples at 32000 Hz between envelope/noise steps)
| Rate | Per | Rate | Per | Rate | Per | Rate | Per |
|---|---|---|---|---|---|---|---|
| $00 | ∞ (stop) | $08 | 384 | $10 | 64 | $18 | 10 |
| $01 | 2048 | $09 | 320 | $11 | 48 | $19 | 8 |
| $02 | 1536 | $0A | 256 | $12 | 40 | $1A | 6 |
| $03 | 1280 | $0B | 192 | $13 | 32 | $1B | 5 |
| $04 | 1024 | $0C | 160 | $14 | 24 | $1C | 4 |
| $05 | 768 | $0D | 128 | $15 | 20 | $1D | 3 |
| $06 | 640 | $0E | 96 | $16 | 16 | $1E | 2 |
| $07 | 512 | $0F | 80 | $17 | 12 | $1F | 1 |
Pattern: periods are {2048,1536,1280} >> n. Timing detail: a single global counter decrements once per sample, wrapping to 30719 ($77FF) below 0; an event fires when `(counter + offset[rate]) % period[rate] == 0` with per-column offsets: rates ≡ 1 (mod 3) → 0, ≡ 2 (mod 3) → 1040, ≡ 0 (mod 3) → 536 (rate 0 never fires).

## Noise generator
- Single shared 15-bit LFSR, initial value -$4000 ($4000 as raw bits) after reset; output range -$4000..+$3FFF replaces the voice's post-Gaussian sample when NON bit set (envelope still applied; pitch and Gaussian filter do NOT apply to noise).
- Stepped at the rate selected by FLG bits 0-4 (same 32-entry period table): `level = ((level >> 1) & $3FFF) | ((bit0 ^ bit1) << 14)`.
- Selected frequencies (rate → Hz): $00 stop, $01 16, $0C 200, $10 500, $14 1.3k, $1C 8k, $1E 16k, $1F 32k (= 32000/period).

## Echo
- Buffer: 4-byte frames at `addr = (ESA*$100 + index*4) & $FFFF` — wraps around the 16-bit ARAM space (can clobber zero page: ESA=0, EDL=0 overwrites $0000-$0003). Frame: L sample (16-bit, bit0 of stored value = 0 → effectively 15-bit), then R sample.
- Length: EDL*512 samples (EDL*2048 bytes); EDL=0 → 1 frame (4 bytes). `index` increments each sample; when index reaches the limit it resets to 0, and the EDL value is (re)latched only when index wraps to 0 → an EDL change can take up to 7680 samples (240 ms) to apply. ESA is read 32 cycles before its write use → ESA changes appear ~1 sample delayed.
- Per sample, per channel (L/R filtered independently, same coefficients):
```
in            = EchoRAM[addr] >> 1                 ; 15-bit read (oldest frame)
buf[i & 7]    = in                                 ; 8-entry FIR history ring
sum  = Σ k=0..6 (buf[(i-7+k) & 7] * FIRk) >> 6     ; 7 taps, NO overflow handling (wraps)
sum += (buf[i & 7] * FIR7) >> 6                    ; last tap saturated to -$8000/+$7FFF
echo_out   = (sum * EVOLx) >> 7                    ; added to main output
echo_input = voices_with_EON + ((sum * EFB) >> 7)  ; feedback
echo_input &= $FFFE                                ; force 15-bit, bit0 = 0
if FLG bit5 clear: EchoRAM[addr] = echo_input      ; write (disabled by FLG.5)
i++; index++ (wrap per EDL)
```
- Echo keeps running when writes are disabled: reads, FIR, EVOL output and position advance all continue (old buffer contents keep playing unless EVOL/FIR are zeroed).
- FIR identity = $7F,0,0,0,0,0,0,0. Keep |Σ FIR| ≤ $80 to avoid the tap-7-only clamp popping. Init order: set FLG bit5, program ESA/EDL/FIR, wait ≥ 240 ms worth of samples, then clear FLG bit5.

## KON/KOFF timing
- KON and KOFF are polled every 2 samples (16000 Hz, every 64 CPU clocks). Two quick writes usually mean only the second is seen.
- Internal KON bits are cleared as part of the poll itself (step 3 of the poll sequence, right after the key-on actions); KON acts edge-like ("on write"), while KOFF and FLG bit7 are level-sensitive until rewritten. If KOFF is cleared within 63 SPC700 cycles of the KON write, the channel is still keyed on normally (both writes land before the same 64-clock poll).
- After key-on there are 5 "empty" samples before envelope updates and BRR decoding begin (KON during KOFF ⇒ keyed on, keyed off 2 samples later ⇒ silence).
- Poll order per voice: (1) if FLG.7 or KOFF bit → Release (FLG.7 also forces env=0); (2) if internal KON bit → key-on actions; (3) clear internal KON.
- KON+KOFF both set: key-on then key-off → fast silence (env reset to 0), may click.

## Per-sample DSP pipeline (32 CPU cycles, T0-T31)
Condensed from the fullsnes low-level chart (3 register-array reads + up to 1 ARAM word access per cycle):
- T0-T22: voices processed in a staggered pipeline. Per voice x (spread over ~5 slots): read SRCN/DIR entry → read BRR header+data bytes → read PITCHL/PITCHH, ADSR1, ADSR2 (or GAIN), VOLL/VOLR → write back ENVX then OUTX → ENDX bit update. FLG bit7 (reset) is checked once per voice per sample; ENDX bits update one bit every 3rd cycle: ENDX.0 at T1, ENDX.1 at T4, ENDX.2 at T7, ... ENDX.7 at T22.
- T23: read echo L from ARAM; FIR0. T24: read echo R; FIR1/FIR2. T25-T26: FIR3-FIR7; V0 BRR fetch.
- T27-T28: MVOLL/EVOLL/EFB, then MVOLR/EVOLR/PMON — output mix.
- T29: NON, EON, DIR, FLG bit5. T30: EDL, ESA, echo L write, (KON). T31: KOFF, FLG, V0ADSR2, echo R write, KON latch.
- Timers tick at T1 (and T17 for the 64 kHz stage).
Consequence: registers are sampled at those fixed points, e.g. echo registers near the end of the loop, KON/KOFF at T30/T31 of every second sample.

## Output mixer (per channel L/R)
```
sum = 0
for v in 0..7: sum = clamp16(sum + ((voice_out[v] * VvVOLx) >> 6))   ; voice_out = OUTX 15-bit
sum = (sum * MVOLx) >> 7
sum = clamp16(sum + ((fir_out * EVOLx) >> 7))
if FLG bit6 (mute): sum = 0
sum = sum ^ $FFFF          ; final phase inversion by the post-amp
```
DAC output: 16-bit stereo, 32000 Hz nominal (measured 32000-32160 Hz per console).
