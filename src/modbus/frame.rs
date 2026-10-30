use crate::FrameInfo;

// ---------- CRC16 Modbus 计算 ----------
pub fn calc_crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &byte in data {
        crc ^= byte as u16;
        for _ in 0..8 {
            if crc & 0x0001 != 0 {
                crc = (crc >> 1) ^ 0xA001;
            } else {
                crc >>= 1;
            }
        }
    }
    crc
}

/// 根据 FrameInfo 构造原始的 Modbus 帧字节（用于 UI 展示）
pub fn frame_bytes_from_info(fi: &FrameInfo) -> Vec<u8> {
    let is_tcp = fi.is_tcp;
    let unit = fi.unit;
    match fi.func_code {
        0x01 | 0x02 => {
            // 读线圈(0x01) / 读离散输入(0x02) 响应
            let mut bytes: Vec<u8> = Vec::new();
            let mut raw = Vec::new();
            raw.push(fi.func_code);
            let bit_count = fi.values.len();
            let byte_count = bit_count.div_ceil(8);
            raw.push(byte_count as u8);
            // 将 bool 值打包为字节
            let mut packed = vec![0u8; byte_count];
            for (i, &v) in fi.values.iter().enumerate() {
                if v != 0 {
                    packed[i / 8] |= 1 << (i % 8);
                }
            }
            raw.extend_from_slice(&packed);
            if is_tcp {
                bytes.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
                bytes.push(0x00);
                bytes.push(raw.len() as u8 + 1);
                bytes.push(unit);
                bytes.extend_from_slice(&raw);
            } else {
                bytes.push(unit);
                bytes.extend_from_slice(&raw);
                let crc = calc_crc16(&bytes);
                bytes.push((crc & 0xFF) as u8);
                bytes.push((crc >> 8) as u8);
            }
            bytes
        }
        0x03 => {
            // 读保持寄存器响应
            let mut bytes: Vec<u8> = Vec::new();
            let mut raw = Vec::new();
            raw.push(fi.func_code);
            let payload_len = fi.values.len() * 2;
            raw.push(payload_len as u8);
            for v in &fi.values {
                raw.push((v >> 8) as u8);
                raw.push((v & 0xFF) as u8);
            }
            if is_tcp {
                bytes.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
                bytes.push(0x00);
                bytes.push(raw.len() as u8 + 1);
                bytes.push(unit);
                bytes.extend_from_slice(&raw);
            } else {
                bytes.push(unit);
                bytes.extend_from_slice(&raw);
                let crc = calc_crc16(&bytes);
                bytes.push((crc & 0xFF) as u8);
                bytes.push((crc >> 8) as u8);
            }
            bytes
        }
        0x04 => {
            // 读输入寄存器响应
            let mut bytes: Vec<u8> = Vec::new();
            let mut raw = Vec::new();
            raw.push(fi.func_code);
            let payload_len = fi.values.len() * 2;
            raw.push(payload_len as u8);
            for v in &fi.values {
                raw.push((v >> 8) as u8);
                raw.push((v & 0xFF) as u8);
            }
            if is_tcp {
                bytes.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
                bytes.push(0x00);
                bytes.push(raw.len() as u8 + 1);
                bytes.push(unit);
                bytes.extend_from_slice(&raw);
            } else {
                bytes.push(unit);
                bytes.extend_from_slice(&raw);
                let crc = calc_crc16(&bytes);
                bytes.push((crc & 0xFF) as u8);
                bytes.push((crc >> 8) as u8);
            }
            bytes
        }
        0x05 => {
            // 写单线圈响应 (echo: addr + 0xFF00/0x0000)
            let mut bytes: Vec<u8> = Vec::new();
            let mut raw = Vec::new();
            let addr = fi.addr;
            raw.push(fi.func_code);
            raw.push((addr >> 8) as u8);
            raw.push((addr & 0xFF) as u8);
            if let Some(&v) = fi.values.first() {
                if v != 0 {
                    raw.push(0xFF);
                    raw.push(0x00);
                } else {
                    raw.push(0x00);
                    raw.push(0x00);
                }
            }
            if is_tcp {
                bytes.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
                bytes.push(0x00);
                bytes.push(raw.len() as u8 + 1);
                bytes.push(unit);
                bytes.extend_from_slice(&raw);
            } else {
                bytes.push(unit);
                bytes.extend_from_slice(&raw);
                let crc = calc_crc16(&bytes);
                bytes.push((crc & 0xFF) as u8);
                bytes.push((crc >> 8) as u8);
            }
            bytes
        }
        0x06 => {
            // 写单寄存器响应
            let mut bytes: Vec<u8> = Vec::new();
            let mut raw = Vec::new();
            let addr = fi.addr;
            raw.push(fi.func_code);
            raw.push((addr >> 8) as u8);
            raw.push((addr & 0xFF) as u8);
            if let Some(v) = fi.values.first() {
                raw.push((v >> 8) as u8);
                raw.push((v & 0xFF) as u8);
            }
            if is_tcp {
                bytes.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
                bytes.push(0x00);
                bytes.push(raw.len() as u8 + 1);
                bytes.push(unit);
                bytes.extend_from_slice(&raw);
            } else {
                bytes.push(unit);
                bytes.extend_from_slice(&raw);
                let crc = calc_crc16(&bytes);
                bytes.push((crc & 0xFF) as u8);
                bytes.push((crc >> 8) as u8);
            }
            bytes
        }
        0x0F => {
            // 写多线圈响应
            let mut bytes: Vec<u8> = Vec::new();
            let addr = fi.addr;
            let qty = fi.values.len() as u16;
            let raw = vec![
                fi.func_code,
                (addr >> 8) as u8,
                (addr & 0xFF) as u8,
                (qty >> 8) as u8,
                (qty & 0xFF) as u8,
            ];
            if is_tcp {
                bytes.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
                bytes.push(0x00);
                bytes.push(raw.len() as u8 + 1);
                bytes.push(unit);
                bytes.extend_from_slice(&raw);
            } else {
                bytes.push(unit);
                bytes.extend_from_slice(&raw);
                let crc = calc_crc16(&bytes);
                bytes.push((crc & 0xFF) as u8);
                bytes.push((crc >> 8) as u8);
            }
            bytes
        }
        0x10 => {
            // 写多寄存器响应
            let mut bytes: Vec<u8> = Vec::new();
            let addr = fi.addr;
            let qty = fi.values.len() as u16;
            let raw = vec![
                fi.func_code,
                (addr >> 8) as u8,
                (addr & 0xFF) as u8,
                (qty >> 8) as u8,
                (qty & 0xFF) as u8,
            ];
            if is_tcp {
                bytes.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
                bytes.push(0x00);
                bytes.push(raw.len() as u8 + 1);
                bytes.push(unit);
                bytes.extend_from_slice(&raw);
            } else {
                bytes.push(unit);
                bytes.extend_from_slice(&raw);
                let crc = calc_crc16(&bytes);
                bytes.push((crc & 0xFF) as u8);
                bytes.push((crc >> 8) as u8);
            }
            bytes
        }
        _ => vec![],
    }
}
