// src/png.rs — минимальный PNG-кодек (RGBA, 8 бит) без внешних зависимостей.
// Писатель: используется HAL для захвата кадров (gpu-present → frame_NNNNN.png,
// а с v0.3 — live-вид редактора /frame.png и MCP-скриншоты).
// Читатель (v0.3): load-texture — декодирует PNG в текстуру «GPU»:
//   deflate stored+fixed+dynamic (свой inflate), фильтры 0-4, цветовые типы
//   0/2/3/4/6 (серый, RGB, палитра, серый+альфа, RGBA), глубина 8, без interlace.

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *slot = c;
    }
    let mut c = 0xFFFF_FFFFu32;
    for &byte in data {
        c = table[((c ^ byte as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &x in data {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn chunk(out: &mut Vec<u8>, tag: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(tag);
    out.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(tag);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

/// RGBA → PNG-байты (stored deflate; писателю сжатие не нужно).
pub fn encode_png(rgba: &[u8], w: usize, h: usize) -> Result<Vec<u8>, String> {
    if rgba.len() < w * h * 4 {
        return Err(format!("png: буфер {} < {}x{}x4", rgba.len(), w, h));
    }
    let mut raw = Vec::with_capacity(h * (w * 4 + 1));
    for y in 0..h {
        raw.push(0u8);
        let start = y * w * 4;
        raw.extend_from_slice(&rgba[start..start + w * 4]);
    }
    let mut z = Vec::with_capacity(raw.len() + raw.len() / 65535 * 5 + 64);
    z.extend_from_slice(&[0x78, 0x01]);
    let mut i = 0usize;
    while i < raw.len() {
        let n = (raw.len() - i).min(65535);
        let last = if i + n >= raw.len() { 1u8 } else { 0u8 };
        z.push(last);
        z.extend_from_slice(&(n as u16).to_le_bytes());
        z.extend_from_slice(&(!(n as u16)).to_le_bytes());
        z.extend_from_slice(&raw[i..i + n]);
        i += n;
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut out = Vec::with_capacity(z.len() + 128);
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8 бит, RGBA
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

pub fn write_png(path: &std::path::Path, rgba: &[u8], w: usize, h: usize) -> Result<(), String> {
    let bytes = encode_png(rgba, w, h)?;
    std::fs::write(path, &bytes).map_err(|e| format!("png {}: {}", path.display(), e))
}

// ——————————————————————————— читатель (v0.3) ———————————————————————————

/// Инфлятор RFC 1951 (stored + fixed + dynamic Huffman). На входе zlib-поток
/// (RFC 1950: 2 байта заголовка + adler32 в конце) — как в PNG IDAT.
fn inflate(z: &[u8]) -> Result<Vec<u8>, String> {
    if z.len() < 6 {
        return Err("png: idat слишком короткий".into());
    }
    if z[0] & 0x0F != 8 {
        return Err(format!("png: только deflate, метод {}", z[0] & 0x0F));
    }
    let mut out: Vec<u8> = Vec::with_capacity(z.len() * 4);
    struct Br<'a> {
        b: &'a [u8],
        pos: usize, // битовая позиция
    }
    impl Br<'_> {
        fn bits(&mut self, n: u32) -> Result<u32, String> {
            let mut v = 0u32;
            for i in 0..n {
                let byte = *self
                    .b
                    .get(self.pos >> 3)
                    .ok_or("png: deflate: конец данных")?;
                let bit = (byte >> (self.pos & 7)) & 1;
                v |= (bit as u32) << i;
                self.pos += 1;
            }
            Ok(v)
        }
        fn align(&mut self) {
            if self.pos & 7 != 0 {
                self.pos += 8 - (self.pos & 7);
            }
        }
    }
    // Канонический Хаффман: len_count → первый код каждого размера → индексы.
    struct Huf {
        counts: [u16; 16],
        symbols: Vec<u16>,
    }
    impl Huf {
        fn build(lengths: &[u8]) -> Huf {
            let mut counts = [0u16; 16];
            for &l in lengths {
                counts[l as usize] += 1;
            }
            counts[0] = 0;
            let mut offs = [0u16; 16];
            for i in 1..16 {
                offs[i] = offs[i - 1] + counts[i - 1];
            }
            let mut symbols = vec![0u16; lengths.len()];
            for (sym, &l) in lengths.iter().enumerate() {
                if l != 0 {
                    symbols[offs[l as usize] as usize] = sym as u16;
                    offs[l as usize] += 1;
                }
            }
            Huf { counts, symbols }
        }
        fn decode(&self, br: &mut Br) -> Result<u16, String> {
            let mut code = 0i32;
            let mut first = 0i32;
            let mut index = 0i32;
            for len in 1..16 {
                code |= br.bits(1)? as i32;
                let count = self.counts[len] as i32;
                if code - count < first {
                    return Ok(self.symbols[(index + (code - first)) as usize]);
                }
                index += count;
                first += count;
                first <<= 1;
                code <<= 1;
            }
            Err("png: huffman: битовый поток испорчен".into())
        }
    }

    const LBASE: [u16; 29] = [
        3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
        131, 163, 195, 227, 258,
    ];
    const LEXT: [u8; 29] = [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
    ];
    const DBASE: [u16; 30] = [
        1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
        2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
    ];
    const DEXT: [u8; 30] = [
        0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
        13, 13,
    ];

    let mut br = Br { b: z, pos: 16 }; // пропускаем zlib-заголовок (2 байта)
    loop {
        let final_ = br.bits(1)?;
        let btype = br.bits(2)?;
        match btype {
            0 => {
                br.align();
                let p = br.pos / 8;
                let len = u16::from_le_bytes([z[p], z[p + 1]]) as usize;
                let nlen = u16::from_le_bytes([z[p + 2], z[p + 3]]) as usize;
                if len != (!nlen & 0xFFFF) {
                    return Err("png: stored: неверный nlen".into());
                }
                out.extend_from_slice(&z[p + 4..p + 4 + len]);
                br.pos += (len + 4) * 8;
            }
            1 | 2 => {
                let (lit, dist) = if btype == 1 {
                    let mut ll = [0u8; 288];
                    for (i, slot) in ll.iter_mut().enumerate() {
                        *slot = match i {
                            0..=143 => 8,
                            144..=255 => 9,
                            256..=279 => 7,
                            _ => 8,
                        };
                    }
                    (Huf::build(&ll), Huf::build(&[5u8; 30]))
                } else {
                    let hlit = br.bits(5)? as usize + 257;
                    let hdist = br.bits(5)? as usize + 1;
                    let hclen = br.bits(4)? as usize + 4;
                    const ORD: [usize; 19] = [
                        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
                    ];
                    let mut cl = [0u8; 19];
                    for i in 0..hclen {
                        cl[ORD[i]] = br.bits(3)? as u8;
                    }
                    let clh = Huf::build(&cl);
                    let mut lengths = vec![0u8; hlit + hdist];
                    let mut i = 0usize;
                    while i < hlit + hdist {
                        let sym = clh.decode(&mut br)?;
                        match sym {
                            0..=15 => {
                                lengths[i] = sym as u8;
                                i += 1;
                            }
                            16 => {
                                if i == 0 {
                                    return Err("png: repeat без предыдущей длины".into());
                                }
                                let prev = lengths[i - 1];
                                let n = 3 + br.bits(2)? as usize;
                                for _ in 0..n {
                                    if i >= lengths.len() {
                                        return Err("png: repeat за границей".into());
                                    }
                                    lengths[i] = prev;
                                    i += 1;
                                }
                            }
                            17 => {
                                let n = 3 + br.bits(3)? as usize;
                                i = (i + n).min(lengths.len());
                            }
                            18 => {
                                let n = 11 + br.bits(7)? as usize;
                                i = (i + n).min(lengths.len());
                            }
                            _ => return Err("png: плохой символ длин".into()),
                        }
                    }
                    (Huf::build(&lengths[..hlit]), Huf::build(&lengths[hlit..]))
                };
                loop {
                    let sym = lit.decode(&mut br)?;
                    match sym {
                        0..=255 => out.push(sym as u8),
                        256 => break,
                        _ => {
                            let si = (sym - 257) as usize;
                            if si >= 29 {
                                return Err("png: плохой len-символ".into());
                            }
                            let len = LBASE[si] as usize + br.bits(LEXT[si] as u32)? as usize;
                            let dsym = dist.decode(&mut br)? as usize;
                            if dsym >= 30 {
                                return Err("png: плохой dist-символ".into());
                            }
                            let d = DBASE[dsym] as usize + br.bits(DEXT[dsym] as u32)? as usize;
                            if d > out.len() {
                                return Err("png: dist уходит до начала".into());
                            }
                            let start = out.len() - d;
                            for k in 0..len {
                                let byte = out[start + k];
                                out.push(byte);
                            }
                        }
                    }
                }
            }
            _ => return Err("png: btype=3 запрещён".into()),
        }
        if final_ == 1 {
            break;
        }
    }
    Ok(out)
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// PNG-байты → (RGBA, w, h). Глубина 8, interlace 0; типы 0/2/3/4/6.
pub fn decode_png(bytes: &[u8]) -> Result<(Vec<u8>, usize, usize), String> {
    const SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if bytes.len() < 8 || bytes[..8] != SIG {
        return Err("png: это не PNG".into());
    }
    let mut pos = 8usize;
    let mut w = 0usize;
    let mut h = 0usize;
    let mut color = 0u8;
    let mut depth = 0u8;
    let mut interlace = 0u8;
    let mut idat: Vec<u8> = Vec::new();
    let mut plte: Vec<u8> = Vec::new();
    let mut trns: Vec<u8> = Vec::new();
    while pos + 8 <= bytes.len() {
        let len = be32(&bytes[pos..pos + 4]) as usize;
        let tag = &bytes[pos + 4..pos + 8];
        let data_start = pos + 8;
        if data_start + len + 4 > bytes.len() {
            return Err("png: чанк обрезан".into());
        }
        let data = &bytes[data_start..data_start + len];
        match tag {
            b"IHDR" => {
                w = be32(data) as usize;
                h = be32(&data[4..]) as usize;
                depth = data[8];
                color = data[9];
                interlace = data[12];
            }
            b"PLTE" => plte = data.to_vec(),
            b"tRNS" => trns = data.to_vec(),
            b"IDAT" => idat.extend_from_slice(data),
            b"IEND" => break,
            _ => {}
        }
        pos = data_start + len + 4; // CRC пропускаем (декодер верифицирует inflate)
    }
    if w == 0 || h == 0 || w * h > 64_000_000 {
        return Err(format!("png: плохой размер {}x{}", w, h));
    }
    if depth != 8 {
        return Err(format!("png: поддерживается глубина 8, а не {}", depth));
    }
    if interlace != 0 {
        return Err("png: interlace не поддерживается".into());
    }
    let channels = match color {
        0 => 1usize,
        2 => 3,
        3 => 1,
        4 => 2,
        6 => 4,
        _ => return Err(format!("png: неизвестный цветовой тип {}", color)),
    };
    let raw = inflate(&idat)?;
    let stride = w * channels;
    if raw.len() < h * (stride + 1) {
        return Err(format!(
            "png: распаковано {} байт, нужно {}",
            raw.len(),
            h * (stride + 1)
        ));
    }
    // unfilter
    let mut lines: Vec<Vec<u8>> = Vec::with_capacity(h);
    let mut prev = vec![0u8; stride];
    for y in 0..h {
        let row_start = y * (stride + 1);
        let ft = raw[row_start];
        let mut row = raw[row_start + 1..row_start + 1 + stride].to_vec();
        for x in 0..stride {
            let a = if x >= channels { row[x - channels] } else { 0 };
            let b = prev[x];
            let c = if x >= channels { prev[x - channels] } else { 0 };
            row[x] = match ft {
                0 => row[x],
                1 => row[x].wrapping_add(a),
                2 => row[x].wrapping_add(b),
                3 => row[x].wrapping_add(((a as u16 + b as u16) / 2) as u8),
                4 => {
                    let p = a as i16 + b as i16 - c as i16;
                    let pa = (p - a as i16).abs();
                    let pb = (p - b as i16).abs();
                    let pc = (p - c as i16).abs();
                    let pred = if pa <= pb && pa <= pc {
                        a
                    } else if pb <= pc {
                        b
                    } else {
                        c
                    };
                    row[x].wrapping_add(pred)
                }
                _ => return Err(format!("png: фильтр {}", ft)),
            };
        }
        lines.push(row.clone());
        prev = row;
    }
    // → RGBA
    let mut out = vec![0u8; w * h * 4];
    for (y, row) in lines.iter().enumerate() {
        for x in 0..w {
            let o = (y * w + x) * 4;
            match color {
                0 => {
                    let g = row[x];
                    out[o] = g;
                    out[o + 1] = g;
                    out[o + 2] = g;
                    out[o + 3] = 255;
                }
                2 => {
                    out[o] = row[x * 3];
                    out[o + 1] = row[x * 3 + 1];
                    out[o + 2] = row[x * 3 + 2];
                    out[o + 3] = 255;
                }
                3 => {
                    let pi = row[x] as usize * 3;
                    if pi + 2 >= plte.len() {
                        return Err("png: палитра короче индексов".into());
                    }
                    out[o] = plte[pi];
                    out[o + 1] = plte[pi + 1];
                    out[o + 2] = plte[pi + 2];
                    out[o + 3] = trns.get(row[x] as usize).copied().unwrap_or(255);
                }
                4 => {
                    let g = row[x * 2];
                    out[o] = g;
                    out[o + 1] = g;
                    out[o + 2] = g;
                    out[o + 3] = row[x * 2 + 1];
                }
                _ => {
                    out[o] = row[x * 4];
                    out[o + 1] = row[x * 4 + 1];
                    out[o + 2] = row[x * 4 + 2];
                    out[o + 3] = row[x * 4 + 3];
                }
            }
        }
    }
    Ok((out, w, h))
}
