# SNES DSP-1 Reference (HLE)

The DSP-1 is a NEC uPD77C25 (uPD7725-family) DSP with a copyrighted, undumpable-in-scope
program+data ROM, used as a 16-bit fixed-point math coprocessor for pseudo-3D / Mode-7
perspective on games such as Super Mario Kart, Pilotwings, Ballz 3D, Suzuka 8 Hours.
Because the firmware ROM is copyrighted, all mainstream emulators (snes9x, bsnes/higan)
run it **HLE**: they reimplement the reverse-engineered COMMAND SET, not the DSP core.

Sources transcribed here: snes.nesdev.org/wiki/DSP-1 and /DSP_Expansion (host interface,
mapping, command list), snes9x `dsp1.cpp` (Andreas Naive / neviksti reverse-engineered
command math), problemkaputt.de/fullsnes.htm (expansion mapping, header). All formulas are
transcribed from the public HLE source; **no math here is invented**. Items that could not
be fully cross-verified are flagged `⚠`.

Only **DSP-1 / DSP-1B** are in scope. DSP-2/3/4 have entirely different command sets and are
out of scope.

---

## 1. Fixed-point conventions

| Name | Format | Meaning |
|---|---|---|
| Q15 / 1.15 | signed 16-bit, `>>15` after a product | value in [-1, +1); `$8000`=-1, `$7FFF`≈+0.99997, `$4000`=+0.5 |
| Integer | signed 16-bit | plain coordinate / world unit |
| Angle | signed 16-bit | full circle = `$10000` (2π). `$4000`=90°, `$8000`=±180°(π), `$C000`=-90°. Sign = direction. |
| Double | signed 32-bit | radius-squared / intermediate accumulators |
| Float pair | (Coefficient Q15, Exponent int16) | value = Coefficient · 2^Exponent; used by Inverse and internal normalization |

Every product of two Q15 (or Q15×coord) values is followed by an arithmetic `>> 15` to
renormalize. Dot products sum three such terms. Saturation is applied only where noted
(Inverse clamps `-32767`; `DSP1_Truncate` saturates on exponent overflow). Rounding: the
HLE truncates (`>>` = floor toward −∞ in C on `int`); Op20/Op38 are `+1` variants of Op00/Op18.

Helper routines used by the math (from `dsp1.cpp`):
- `DSP1_Sin(a)` / `DSP1_Cos(a)`: Q15 sine/cosine of a 16-bit angle, via `DSP1_SinTable`
  (256 signed-16 entries covering 0…π/2, mirrored/negated for the full circle) with
  `DSP1_MulTable` interpolation.
- `DSP1_Normalize(v,&C,&E)`: shift `v` left until in `[$4000,$7FFF]`, returning Q15 `C` and exponent `E`.
- `DSP1_NormalizeDouble(v32,&C,&E)`: same for a 32-bit input.
- `DSP1_Truncate(C,E)`: denormalize Q15 `C` by exponent `E` back to an int16, saturating.
- `DSP1_Inverse(...)`: see Op10 below (also used internally by Parameter/Project/Raster/Target/Gyrate).
- `DSP1ROM[1024]`: internal data-ROM table. Offsets used: `$0021–$002F` normalize mults,
  `$0031–$003F` denormalize shifts, `$0065…` 256-entry reciprocal seed table,
  `$00D5…` square-root node table, `$0324–$0328` zenith-clip correction coefficients.

---

## 2. Host interface

The SNES talks to the DSP-1 through two memory-mapped byte ports:

- **DR — Data Register** (bidirectional): the CPU writes the command byte, then the parameter
  bytes; then reads the result bytes. All transfers are one byte at a time, in the fixed order
  defined per command (low byte then high byte of each 16-bit word).
- **SR — Status Register** (read-only): bit 7 = **RQM** (Request-for-Master / "Data Ready").
  `RQM=1` (`$80`) means the DSP is ready to be written or has a byte ready to read; `RQM=0`
  means busy. The uPD7725 SR also has DRC (data-register 8/16 mode), DMA, RQM-source bits at
  lower positions, but the SNES DSP-1 protocol only observes **bit 7**.
  HLE completes each command instantly, so a correct HLE returns `$80` (ready) whenever
  it has a byte pending and after each accepted byte; many emulators simply return `$80`
  constantly (alternately toggling to model handshake is not required for these games).

### Protocol
1. CPU writes 1 command byte to DR (see §4 for the byte→command map; several byte aliases
   select the same command).
2. CPU writes the command's *N* parameter bytes to DR (see the "In" column, ×2 = bytes).
3. CPU reads the command's *M* result bytes from DR (see the "Out" column, ×2 = bytes).
4. After all result bytes are consumed the DSP is idle again (RQM=1) and awaits the next
   command byte. Commands taking 0 parameters (none here) or 0 results (the Attitude Ops)
   skip the corresponding step.

The 16-bit words are transferred little-endian (low byte first) across the byte port.

### Memory mapping
The DR/SR ports appear at fixed addresses that depend on the board / map mode (from the
cartridge header, §5). Two families exist:

| Family | Banks | DR (Data) | SR (Status) | Decode |
|---|---|---|---|---|
| **LoROM / Mode 20** (most DSP-1 games: SMK, Pilotwings) | `$30–$3F` and mirror `$B0–$BF` | `$8000–$BFFF` | `$C000–$FFFF` | within `A15=1`, `A14=0`→DR, `A14=1`→SR |
| **HiROM / Mode 21** | `$00–$0F` and mirror `$80–$8F` | `$6000–$6FFF` | `$7000–$7FFF` | `$6xxx`→DR, `$7xxx`→SR |

Within each range every address mirrors the same port (only the DR/SR distinction matters).
The nesdev DSP-1 page lists the LoROM DR span as `$308000–$3FBFFF` and SR as `$30C000–$3FFFFF`
(plus `$B0…` mirror), and the HiROM span as `$006000–$0F6FFF` (DR) / `$007000–$0F7FFF` (SR).

> ⚠ The task brief described "LoROM DR `$6000`/SR `$7000` in banks `$30–$3F`". That conflates
> the two families: `$6000/$7000` is the **HiROM/Mode-21** placement; canonical LoROM DSP-1
> uses `$8000/$C000` in `$30–$3F`. The implementation should choose the port addresses from
> the header **map mode**, not hard-code one. Both are documented above.

---

## 3. Internal state persisted across commands

Attitude commands install a 3×3 Q15 matrix; Parameter installs the full camera/projection
state; these persist and are consumed by later commands and must be part of the DSP-1 struct
(and therefore serde-serialized in the save state):

- `matrixA`, `matrixB`, `matrixC` — three 3×3 Q15 rotation matrices (Attitude A/B/C).
- Camera state from Op02 Parameter: `SinAas,CosAas,SinAzs,CosAzs`, normal `Nx,Ny,Nz`,
  centre `CentreX,CentreY`, gaze origin `Gx,Gy,Gz`, `C_Les,E_Les,G_Les`, `VPlane_C,VPlane_E`,
  `SinAZS,CosAZS`, `SecAZS_C1,SecAZS_E1,SecAZS_C2,SecAZS_E2`, `VOffset`.
- `Op0AVS` — Raster scanline counter, auto-incremented each Op0A call.
- Polar (Op1C) keeps its `…BR`/`…AR` running-register state across calls.

---

## 4. Command set

Byte→command dispatch (multiple bytes alias to the same handler; only the low 6 bits +
a couple of mode bits are decoded). "In"/"Out" are 16-bit **words** unless noted; bytes = words×2.

| Cmd byte(s) | Name | In (words) | Out (words) | Summary |
|---|---|---|---|---|
| `$00` | Multiply | 2 | 1 | `R = (A·B) >> 15` (Q15×Q15→Q15) |
| `$20` | Multiply+1 | 2 | 1 | `R = ((A·B) >> 15) + 1` (rounding variant of `$00`) |
| `$10`,`$30` | Inverse | 2 | 2 | float reciprocal `1/(C·2^E)` |
| `$04`,`$24` | Triangle (Sin/Cos) | 2 | 2 | `Sin=sin(A)·r>>15`, `Cos=cos(A)·r>>15` |
| `$08` | Radius | 3 | 2(=int32) | `(X²+Y²+Z²)<<1` |
| `$18` | Range | 4 | 1 | `(X²+Y²+Z²−R²) >> 15` |
| `$38` | Range+1 | 4 | 1 | `$18` result `+1` |
| `$28` | Distance | 3 | 1 | `√(X²+Y²+Z²)` |
| `$0C`,`$2C` | Rotate (2D) | 3 | 2 | rotate (X,Y) by angle |
| `$1C`,`$3C` | Polar (3D rotate) | 6 | 3 | rotate vector by Z,Y,X angles |
| `$02`,`$12`,`$22`,`$32` | Parameter (camera) | 7 | 4 | set projection; return Vof,Vva,Cx,Cy |
| `$06`,`$16`,`$26`,`$36` | Project | 3 | 3 | 3D→screen H,V,M |
| `$0E`,`$1E`,`$2E`,`$3E` | Target | 2 | 2 | screen H,V→world X,Y |
| `$0A`,`$1A`,`$2A`,`$3A` | Raster | 1 | 4 | per-scanline Mode-7 A,B,C,D; VS auto-inc |
| `$01`,`$05`,`$31`,`$35` | Attitude A | 4 | 0 | build `matrixA` |
| `$11`,`$15` | Attitude B | 4 | 0 | build `matrixB` |
| `$21`,`$25` | Attitude C | 4 | 0 | build `matrixC` |
| `$0D`,`$09`,`$39`,`$3D` | Objective A | 3 | 3 | `F,L,U = matrixA · (X,Y,Z)` |
| `$1D`,`$19` | Objective B | 3 | 3 | via `matrixB` |
| `$2D`,`$29` | Objective C | 3 | 3 | via `matrixC` |
| `$03`,`$33` | Subjective A | 3 | 3 | `X,Y,Z = matrixAᵀ · (F,L,U)` |
| `$13` | Subjective B | 3 | 3 | via `matrixB` |
| `$23` | Subjective C | 3 | 3 | via `matrixC` |
| `$0B`,`$3B` | Scalar A | 3 | 1 | dot with row 0 of `matrixA` |
| `$1B` | Scalar B | 3 | 1 | row 0 of `matrixB` |
| `$2B` | Scalar C | 3 | 1 | row 0 of `matrixC` |
| `$14`,`$34` | Gyrate | 6 | 3 | integrate attitude by angular velocity |
| `$0F`,`$07` | RAM/Memory test | 1 | 1 | returns `$0000` (pass) |
| `$2F`,`$27` | Memory size/dump status | 1 | 1 | returns `$0100` |
| `$1F` | ROM dump | 1 | 1024 | dumps internal `DSP1ROM` (2048 bytes) |

> ⚠ The DSP-1 has **no separate "8-bit multiply" command**. Op00/Op20 are the only multiplies
> (16×16→Q15). The 8-bit-multiply framing belongs to DSP-2/3/4, which are out of scope.

### 4.1 Arithmetic

**Op00 Multiply** — `In: Multiplicand, Multiplier` (Q15). `Out: R = Multiplicand·Multiplier >> 15`.
**Op20** identical but `R + 1`.

**Op10 Inverse** — `In: Coefficient (Q15), Exponent`. `Out: iCoefficient (Q15), iExponent`.
Computes the float reciprocal `1/(Coefficient·2^Exponent)` normalized:
```c
if (Coefficient == 0) { iCoeff = 0x7FFF; iExp = 0x002F; }
else {
  Sign = 1;
  if (Coefficient < 0) { if (Coefficient < -32767) Coefficient = -32767;
                         Coefficient = -Coefficient; Sign = -1; }
  while (Coefficient < 0x4000) { Coefficient <<= 1; Exponent--; }   // normalize to [0x4000,0x7FFF]
  if (Coefficient == 0x4000)
      iCoeff = (Sign==1) ? 0x7FFF : -0x4000, (Sign==-1 ? Exponent-- : 0);
  else {
      i = DSP1ROM[((Coefficient - 0x4000) >> 7) + 0x0065];          // seed
      i = (i + (-i * (Coefficient*i>>15) >>15)) << 1;               // Newton–Raphson x2
      i = (i + (-i * (Coefficient*i>>15) >>15)) << 1;
      iCoeff = i * Sign;
  }
  iExp = 1 - Exponent;
}
```

**Op04 Triangle** — `In: Angle, Radius`. `Out: Sin = sin(Angle)·Radius >> 15`,
`Cos = cos(Angle)·Radius >> 15`. Angle in the `$10000`=2π scale.

**Op08 Radius** — `In: X,Y,Z` (int16). `Out: 32-bit (X²+Y²+Z²) << 1` returned low word then high word.

**Op18 Range** — `In: X,Y,Z,R`. `Out: (X²+Y²+Z² − R²) >> 15`. **Op38** adds 1.

**Op28 Distance** — `In: X,Y,Z`. `Out: R = √(X²+Y²+Z²)`:
```c
Radius = X*X + Y*Y + Z*Z;                    // int32
if (Radius == 0) R = 0;
else {
  DSP1_NormalizeDouble(Radius, &C, &E);
  if (E & 1) C = C*0x4000 >> 15;             // odd-exponent half-shift
  Pos   = C*0x0040 >> 15;                     // sqrt table index
  Node1 = DSP1ROM[0x00D5 + Pos];
  Node2 = DSP1ROM[0x00D6 + Pos];
  R = ((Node2 - Node1) * (C & 0x1FF) >> 9) + Node1;   // lerp
  R >>= (E >> 1);                             // halve exponent (sqrt)
}
```

### 4.2 Rotation

**Op0C Rotate (2D)** — `In: Angle, X1, Y1`. `Out:`
```
X2 = (Y1·sinA >> 15) + (X1·cosA >> 15)
Y2 = (Y1·cosA >> 15) − (X1·sinA >> 15)
```

**Op1C Polar (3D)** — `In: Zangle, Yangle, Xangle, X, Y, Z` (running registers `xBR,yBR,zBR`).
`Out: X,Y,Z` after three sequential 2D rotations Z→Y→X:
```
// about Z (updates xBR,yBR):
x1 = (yBR·sinZ>>15)+(xBR·cosZ>>15);  y1 = (yBR·cosZ>>15)-(xBR·sinZ>>15);  xBR=x1; yBR=y1;
// about Y (updates xAR,zBR):
z1 = (xBR·sinY>>15)+(zBR·cosY>>15);  x1 = (xBR·cosY>>15)-(zBR·sinY>>15);  xAR=x1; zBR=z1;
// about X (updates yAR,zAR):
y1 = (zBR·sinX>>15)+(yBR·cosX>>15);  z1 = (zBR·cosX>>15)-(yBR·sinX>>15);  yAR=y1; zAR=z1;
// output = (xAR, yAR, zAR)
```

### 4.3 Attitude matrices (Op01 / Op11 / Op21 = A / B / C)

`In: m (scale, Q15), Zr, Yr, Xr (angles)`. `Out: none` — installs the matrix. Body (matrixA
shown; B and C are identical into `matrixB`/`matrixC`). This is a ZYX Euler composition with
the overall scale `m` pre-halved:
```c
Sz=sin(Zr); Cz=cos(Zr); Sy=sin(Yr); Cy=cos(Yr); Sx=sin(Xr); Cx=cos(Xr);
m >>= 1;
A[0][0]=(m*Cz>>15)*Cy>>15;                         A[0][1]=-((m*Sz>>15)*Cy>>15);                    A[0][2]=m*Sy>>15;
A[1][0]=((m*Sz>>15)*Cx>>15)+(((m*Cz>>15)*Sx>>15)*Sy>>15);
A[1][1]=((m*Cz>>15)*Cx>>15)-(((m*Sz>>15)*Sx>>15)*Sy>>15);
A[1][2]=-((m*Sx>>15)*Cy>>15);
A[2][0]=((m*Sz>>15)*Sx>>15)-(((m*Cz>>15)*Cx>>15)*Sy>>15);
A[2][1]=((m*Cz>>15)*Sx>>15)+(((m*Sz>>15)*Cx>>15)*Sy>>15);
A[2][2]=(m*Cx>>15)*Cy>>15;
```

**Objective (Op0D/1D/2D = A/B/C)** — `In: X,Y,Z`. `Out: F,L,U = matrix · (X,Y,Z)`:
```
F = (X·A[0][0]>>15)+(Y·A[0][1]>>15)+(Z·A[0][2]>>15)
L = (X·A[1][0]>>15)+(Y·A[1][1]>>15)+(Z·A[1][2]>>15)
U = (X·A[2][0]>>15)+(Y·A[2][1]>>15)+(Z·A[2][2]>>15)
```

**Subjective (Op03/13/23 = A/B/C)** — `In: F,L,U`. `Out: X,Y,Z = matrixᵀ · (F,L,U)`
(reads columns instead of rows — inverse of Objective for an orthonormal matrix):
```
X = (F·A[0][0]>>15)+(L·A[1][0]>>15)+(U·A[2][0]>>15)
Y = (F·A[0][1]>>15)+(L·A[1][1]>>15)+(U·A[2][1]>>15)
Z = (F·A[0][2]>>15)+(L·A[1][2]>>15)+(U·A[2][2]>>15)
```

**Scalar / dot (Op0B/1B/2B = A/B/C)** — `In: X,Y,Z`. `Out:`
`S = (X·A[0][0] + Y·A[0][1] + Z·A[0][2]) >> 15` (dot with row 0; note the single `>>15`
applied to the whole sum, not per term).

**Op14 Gyrate** — `In: Zr, Yr, Xr, U, F, L` (current attitude angles + angular-velocity vector).
`Out: Zrr, Xrr, Yrr` (updated angles). Uses secant = 1/cos(Xr):
```c
DSP1_Inverse(cos(Xr), 0, &CSec, &ESec);
// Z:
DSP1_NormalizeDouble(U*cos(Yr) - F*sin(Yr), &C, &E);  E = ESec - E;
DSP1_Normalize(C*CSec>>15, &C, &E);   Zrr = Zr + DSP1_Truncate(C,E);
// X:
Xrr = Xr + (U*sin(Yr)>>15) + (F*cos(Yr)>>15);
// Y:
DSP1_NormalizeDouble(U*cos(Yr) + F*sin(Yr), &C, &E);  E = ESec - E;
DSP1_Normalize(sin(Xr), &CSin, &E);   CTan = CSec*CSin>>15;
DSP1_Normalize(-(C*CTan>>15), &C, &E);
Yrr = Yr + DSP1_Truncate(C,E) + L;
```

### 4.4 Projection (Mode-7 / perspective) — the flagship path

**Op02 Parameter** — `In: Fx, Fy, Fz (viewer world pos), Lfe (focal length), Les (screen
distance), Aas (azimuth angle), Azs (zenith angle)`. `Out: Vof (vertical offset), Vva
(vanishing-point vert), Cx, Cy (screen centre in world)`. Establishes the camera; consumed by
Project/Target/Raster. Full body:
```c
static const int16 MaxAZS_Exp[16] = {0x38B4,0x38B7,0x38BA,0x38BE,0x38C0,0x38C4,0x38C7,0x38CA,
                                     0x38CE,0x38D0,0x38D4,0x38D7,0x38DA,0x38DD,0x38E0,0x38E4};
SinAas=sin(Aas); CosAas=cos(Aas); SinAzs=sin(Azs); CosAzs=cos(Azs);
Nx = SinAzs*-SinAas>>15;  Ny = SinAzs*CosAas>>15;  Nz = CosAzs*0x7FFF>>15;   // view normal
CentreX = Fx + (Lfe*Nx>>15);  CentreY = Fy + (Lfe*Ny>>15);  CentreZ = Fz + (Lfe*Nz>>15);
Gx = CentreX-(Les*Nx>>15);  Gy = CentreY-(Les*Ny>>15);  Gz = CentreZ-(Les*Nz>>15);  // gaze origin
DSP1_Normalize(Les,&C_Les,&E_Les);  G_Les = Les;
DSP1_Normalize(CentreZ,&VPlane_C,&VPlane_E);
MaxAZS = MaxAZS_Exp[-VPlane_E];                                       // clip zenith to horizon
AZS = Azs;  if (AZS<0){ MaxAZS=-MaxAZS; if (AZS<MaxAZS+1) AZS=MaxAZS+1; } else if (AZS>MaxAZS) AZS=MaxAZS;
SinAZS=sin(AZS); CosAZS=cos(AZS);
DSP1_Inverse(CosAZS,0,&SecAZS_C1,&SecAZS_E1);
DSP1_Normalize(VPlane_C*SecAZS_C1>>15,&C,&E);  E += SecAZS_E1;
C = DSP1_Truncate(C,E)*SinAZS>>15;
CentreX += C*SinAas>>15;  CentreY -= C*CosAas>>15;
Cx = CentreX;  Cy = CentreY;  Vof = 0;
if (Azs!=AZS || Azs==MaxAZS) {                                        // horizon over-tilt correction
   if (Azs==-32768) Azs=-32767;
   C = Azs-MaxAZS;  if (C>=0) C--;  Aux = ~(C<<2);
   C = Aux*DSP1ROM[0x0328]>>15;  C = (C*Aux>>15)+DSP1ROM[0x0327];
   Vof -= (C*Aux>>15)*Les>>15;
   C = Aux*Aux>>15;  Aux = (C*DSP1ROM[0x0324]>>15)+DSP1ROM[0x0325];
   CosAZS += (C*Aux>>15)*CosAZS>>15;
}
VOffset = Les*CosAZS>>15;
DSP1_Inverse(SinAZS,0,&CSec,&E);  DSP1_Normalize(VOffset,&C,&E);  DSP1_Normalize(C*CSec>>15,&C,&E);
if (C==-32768){ C>>=1; E++; }  Vva = DSP1_Truncate(-C,E);
DSP1_Inverse(CosAZS,0,&SecAZS_C2,&SecAZS_E2);
```

**Op06 Project** — `In: X,Y,Z (world point)`. `Out: H, V (screen px), M (perspective scale/128)`.
The perspective divide is the `DSP1_Inverse(C10,…)` of accumulated depth:
```c
DSP1_NormalizeDouble((int32)X-Gx,&Px,&E4);  DSP1_NormalizeDouble((int32)Y-Gy,&Py,&E);
DSP1_NormalizeDouble((int32)Z-Gz,&Pz,&E3);
Px>>=1;E4--; Py>>=1;E--; Pz>>=1;E3--;
refE = min(E,E3,E4);
Px=DSP1_ShiftR(Px,E4-refE); Py=DSP1_ShiftR(Py,E-refE); Pz=DSP1_ShiftR(Pz,E3-refE);
C12 = -(Px*Nx>>15) - (Py*Ny>>15) - (Pz*Nz>>15);           // depth along view normal
aux4=C12; refE=16-refE; aux4 = refE>=0 ? aux4<<refE : aux4>>-refE; if (aux4==-1) aux4=0; aux4>>=1;
aux = (uint16)G_Les + aux4;  DSP1_NormalizeDouble(aux,&C10,&E2);  E2 = 15-E2;
DSP1_Inverse(C10,0,&C4,&E4);  C2 = C4*C_Les>>15;          // 1/depth · screen dist
// Horizontal:
C17 = (Px*(CosAas*0x7FFF>>15)>>15) + (Py*(SinAas*0x7FFF>>15)>>15);
C18 = C17*C2>>15;  DSP1_Normalize(C18,&C19,&E7);
H = DSP1_Truncate(C19, E_Les-E2+refE+E7);
// Vertical:
C24 = (Px*(CosAzs*-SinAas>>15)>>15) + (Py*(CosAzs*CosAas>>15)>>15) + (Pz*(-SinAzs*0x7FFF>>15)>>15);
C26 = C24*C2>>15;  DSP1_Normalize(C26,&C25,&E6);
V = DSP1_Truncate(C25, E_Les-E2+refE+E6);
// Scale:
DSP1_Normalize(C2,&C6,&E4);  M = DSP1_Truncate(C6, E4+E_Les-E2-7);
```

**Op0E Target** — inverse of Project. `In: H, V (screen px)`. `Out: X, Y (world on target plane)`:
```c
DSP1_Inverse((V*SinAzs>>15)+VOffset, 8, &C, &E);  E += VPlane_E;
C1 = C*VPlane_C>>15;  E1 = E + SecAZS_E1;
H <<= 8;  DSP1_Normalize(C1,&C,&E);  C = DSP1_Truncate(C,E)*H>>15;
X = CentreX + (C*CosAas>>15);  Y = CentreY - (C*SinAas>>15);
V <<= 8;  DSP1_Normalize(C1*SecAZS_C1>>15,&C,&E1);  C = DSP1_Truncate(C,E1)*V>>15;
X += C*-SinAas>>15;  Y += C*CosAas>>15;
```

**Op0A Raster** — `In: Vs (scanline)`. `Out: A, B, C, D` Mode-7 affine coefficients for that
line; the internal `Op0AVS` scanline counter auto-increments after each call:
```c
DSP1_Inverse((Vs*SinAzs>>15)+VOffset, 7, &C, &E);  E += VPlane_E;
C1 = C*VPlane_C>>15;  E1 = E + SecAZS_E2;
DSP1_Normalize(C1,&C,&E);  C = DSP1_Truncate(C,E);
A = C*CosAas>>15;   Cc = C*SinAas>>15;
DSP1_Normalize(C1*SecAZS_C2>>15,&C,&E1);  C = DSP1_Truncate(C,E1);
B = C*-SinAas>>15;  D = C*CosAas>>15;
// output order: A, B, Cc, D ; then Op0AVS++
```

### 4.5 Status / memory commands

- **Op0F / Op07 (RAM/Memory test)** — `In: 1 word` (ignored/size). `Out: $0000` (pass).
- **Op2F / Op27 (Memory size)** — `In: 1 word`. `Out: $0100`.
- **Op1F (ROM dump)** — `In: 1 word`. `Out: 1024 words` = the internal `DSP1ROM` table
  (2048 bytes), used by test code to read back the sin/reciprocal/sqrt tables.

---

## 5. Cartridge detection

DSP presence is read from the SNES internal header (LoROM base `$7FC0`, HiROM base `$FFC0`):

- **Header byte `$16` = cartridge/chipset type** (absolute `$7FD6` LoROM / `$FFD6` HiROM):
  | Value | Meaning |
  |---|---|
  | `$03` | ROM + Co-processor (DSP), no RAM |
  | `$04` | ROM + Co-processor + RAM |
  | `$05` | ROM + Co-processor + RAM + battery |

  The **co-processor family** is the high nibble of byte `$16` (valid on extended headers):
  `$0x`=DSP, `$1x`=SuperFX/GSU, `$2x`=OBC1, `$3x`=SA-1, `$4x`=S-DD1, `$5x`=S-RTC, `$Ex`=Other,
  `$Fx`=Custom. So DSP boards read `$03`/`$04`/`$05`.

- Byte `$15` = **map mode** (`$20`=LoROM/Mode20, `$21`=HiROM/Mode21) selects which DR/SR
  placement in §2 to use.

- **DSP variant (DSP-1 vs 1B vs 2/3/4) is NOT encoded in the header.** It is resolved by a
  per-game title/database lookup. When ambiguous, **default to DSP-1B**. DSP-1B is a firmware
  revision of DSP-1 that fixes a handful of math bugs (notably in the range/inverse edge
  cases); the reverse-engineered command math above (snes9x `dsp1.cpp`) reflects **DSP-1B**
  behavior, which is what these games ship with. Only DSP-1/1B are in scope.

---

## 6. Game relevance

- **Super Mario Kart** — Mode-7 track perspective: heavy use of **Op02 Parameter**, **Op0A
  Raster** (per-scanline affine A/B/C/D), and **Op06 Project** / **Op0E Target**. Also the
  2D **Op0C Rotate**. This is the hottest path; correctness of Parameter+Raster+Project
  governs whether the track renders.
- **Pilotwings** — flight/attitude math: **Op01/11/21 Attitude**, **Op0D/03 Objective/
  Subjective**, **Op14 Gyrate**, **Op1C Polar**, **Op06 Project**, plus **Op28 Distance** /
  **Op18 Range** for altitude/targeting.
- **Op00/Op20 Multiply**, **Op10 Inverse**, **Op04 Triangle**, **Op08 Radius** — general
  utility, used broadly but simple.
- **Rarely used / test-only:** **Op0F/07**, **Op2F/27**, **Op1F** (memory/status/ROM-dump
  self-tests, mostly at boot).

---

## 7. Verification status

| Command | Math source | Confidence |
|---|---|---|
| Op00/Op20 Multiply | dsp1.cpp | high |
| Op10 Inverse | dsp1.cpp (full body) | high |
| Op04 Triangle | dsp1.cpp | high |
| Op08 Radius, Op18/38 Range | dsp1.cpp | high |
| Op28 Distance | dsp1.cpp (full body) | high |
| Op0C Rotate, Op1C Polar | dsp1.cpp (full body) | high |
| Op01/11/21 Attitude | dsp1.cpp (full body) | high |
| Op0D/1D/2D Objective, Op03/13/23 Subjective, Op0B/1B/2B Scalar | dsp1.cpp (full body) | high |
| Op02 Parameter | dsp1.cpp (full body) | high — depends on `DSP1ROM[$0324–$0328]` and `MaxAZS_Exp[]` constants (transcribed) |
| Op06 Project, Op0E Target, Op0A Raster | dsp1.cpp (full body) | high |
| Op14 Gyrate | dsp1.cpp (full body) | high |
| Op0F/07, Op2F/27 return values | dsp1.cpp | high |
| Op1F ROM dump payload | requires the full `DSP1ROM[1024]` table (only offsets used above are documented; the complete table must be transcribed from dsp1.cpp when Op1F is implemented) | ⚠ partial |

**Flagged for implementation:**
- The **`DSP1ROM[1024]` data table** (reciprocal seeds `$0065…`, sqrt nodes `$00D5…`,
  clip coeffs `$0324–$0328`) and the **`DSP1_SinTable[256]` / `DSP1_MulTable[256]`** tables
  are needed verbatim from `dsp1.cpp` for Distance/Inverse/Parameter/Triangle/Op1F. They are
  large numeric tables not reproduced here — copy them directly from the snes9x source during
  implementation and unit-test against the vectors below.
- **Op1F ROM dump** payload is only correct once the full `DSP1ROM` table is present (⚠).
- The **8-bit multiply** requested in the brief does not exist on DSP-1 (DSP-2/3/4 only) — do
  not implement it here.

### Suggested unit-test vectors (hardware-independent, derivable from the formulas)
- Op00: `A=$4000 (0.5), B=$4000 (0.5) → R=$2000 (0.25)`; `A=$7FFF,B=$7FFF → $7FFE`.
- Op20: same inputs as Op00, result `+1`.
- Op04: `Angle=$0000 → Sin=0, Cos=Radius`; `Angle=$4000 (90°) → Sin=Radius, Cos≈0`.
- Op0C: `Angle=$4000, (X1,Y1)=(r,0) → (X2,Y2)≈(0,-r)` (per the sign convention above).
- Op08: `(X,Y,Z)=(1,2,2) → (1+4+4)<<1 = 18 = $00000012`.
- Op18: `(X,Y,Z,R)=(0,0,0,0) → 0`.
- Op10: `Coefficient=$4000 (0.5), Exp=0 → reciprocal ≈ 2.0` (iCoeff=$4000, iExp=2 path);
  `Coefficient=0 → iCoeff=$7FFF, iExp=$2F`.
These follow directly from the transcribed math; validate the table-driven commands
(Op28 Distance, Op02/06/0A/0E projection) after the numeric tables are copied in.
