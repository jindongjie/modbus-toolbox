    use super::*;
    use crate::AppState;
    use std::sync::Arc;
    use tokio::sync::{mpsc, oneshot, RwLock};

    /// 辅助函数：创建一个带 n 个寄存器的测试 AppState
    fn test_state(holding: usize, coils: usize, discrete: usize, input: usize) -> AppState {
        AppState {
            holding: vec![0u16; holding],
            holding_label: vec!["".to_string(); holding],
            coils: vec![false; coils],
            discrete: vec![false; discrete],
            input_registers: vec![0u16; input],
            last_frame: None,
            is_tcp: false,
            monitor: crate::MonitorStats::default(),
            stability_test_running: false,
            stability_stats: (0, 0, 0),
            reg_change_history: Vec::new(),
            reg_just_changed: vec![false; holding],
            reg_change_direction: vec![crate::ChangeDirection::Up; holding],
            holding_change_enabled: vec![false; holding],
            input_change_enabled: vec![false; input],
            holding_change_patterns: vec![crate::RegChangePattern::Random; holding],
            input_change_patterns: vec![crate::RegChangePattern::Random; input],
            holding_pattern_freqs: vec![1.0; holding],
            input_pattern_freqs: vec![1.0; input],
            holding_pattern_phases: vec![0.0; holding],
            input_pattern_phases: vec![0.0; input],
            holding_pattern_duties: vec![0.5; holding],
            input_pattern_duties: vec![0.5; input],
            read_enabled: [false, false, false, false],
            slave_scan_result: None,
            slave_scan_running: false,
            reg_bar_history: vec![Vec::new(); holding],
            reg_format: crate::RegDataFormat {
                data_type: crate::RegDataType::Uint,
                width: crate::RegDataWidth::Bits16,
            },
            holding_swap_bytes: std::collections::HashMap::new(),
            holding_swap_words: std::collections::HashMap::new(),
            input_swap_bytes: std::collections::HashMap::new(),
            input_swap_words: std::collections::HashMap::new(),
            holding_combinations: std::collections::HashMap::new(),
            input_combinations: std::collections::HashMap::new(),
            io_status: None,
            io_fail_streak: 0,
            last_poll_ms: 0,
            version: 0,
        }
    }

    // ─── 读/写保持寄存器 ───

    #[tokio::test]
    async fn test_read_holding_ok() {
        let state = Arc::new(RwLock::new(test_state(10, 0, 0, 0)));
        {
            let mut s = state.write().await;
            s.holding[3] = 42;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(reg_worker_loop(Arc::clone(&state), rx));

        let (resp_tx, resp_rx) = oneshot::channel();
        tx.send(RegCmd::ReadHolding {
            addr: 3,
            cnt: 2,
            resp: resp_tx,
        })
        .unwrap();
        let result = resp_rx.await.unwrap().unwrap();
        assert_eq!(result, vec![42u16, 0]);
    }

    #[tokio::test]
    async fn test_read_holding_oob() {
        let state = Arc::new(RwLock::new(test_state(5, 0, 0, 0)));
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(reg_worker_loop(Arc::clone(&state), rx));

        let (resp_tx, resp_rx) = oneshot::channel();
        tx.send(RegCmd::ReadHolding {
            addr: 3,
            cnt: 5,
            resp: resp_tx,
        })
        .unwrap();
        let err = resp_rx.await.unwrap().unwrap_err();
        assert_eq!(err, ExceptionCode::IllegalDataAddress);
    }

    #[tokio::test]
    async fn test_write_single_holding() {
        let state = Arc::new(RwLock::new(test_state(5, 0, 0, 0)));
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(reg_worker_loop(Arc::clone(&state), rx));

        let (resp_tx, resp_rx) = oneshot::channel();
        tx.send(RegCmd::WriteSingleHolding {
            addr: 2,
            value: 1234,
            resp: resp_tx,
        })
        .unwrap();
        resp_rx.await.unwrap().unwrap();
        assert_eq!(state.read().await.holding[2], 1234);
    }

    #[tokio::test]
    async fn test_write_multiple_holding() {
        let state = Arc::new(RwLock::new(test_state(10, 0, 0, 0)));
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(reg_worker_loop(Arc::clone(&state), rx));

        let values = vec![10, 20, 30];
        let (resp_tx, resp_rx) = oneshot::channel();
        tx.send(RegCmd::WriteMultipleHolding {
            addr: 1,
            values,
            resp: resp_tx,
        })
        .unwrap();
        resp_rx.await.unwrap().unwrap();
        let s = state.read().await;
        assert_eq!(s.holding[1], 10);
        assert_eq!(s.holding[2], 20);
        assert_eq!(s.holding[3], 30);
    }

    // ─── 读/写线圈 ───

    #[tokio::test]
    async fn test_read_coils_ok() {
        let state = Arc::new(RwLock::new(test_state(0, 8, 0, 0)));
        {
            let mut s = state.write().await;
            s.coils[1] = true;
            s.coils[3] = true;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(reg_worker_loop(Arc::clone(&state), rx));

        let (resp_tx, resp_rx) = oneshot::channel();
        tx.send(RegCmd::ReadCoils {
            addr: 0,
            cnt: 4,
            resp: resp_tx,
        })
        .unwrap();
        let result = resp_rx.await.unwrap().unwrap();
        assert_eq!(result, vec![false, true, false, true]);
    }

    #[tokio::test]
    async fn test_write_single_coil() {
        let state = Arc::new(RwLock::new(test_state(0, 5, 0, 0)));
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(reg_worker_loop(Arc::clone(&state), rx));

        let (resp_tx, resp_rx) = oneshot::channel();
        tx.send(RegCmd::WriteSingleCoil {
            addr: 2,
            value: true,
            resp: resp_tx,
        })
        .unwrap();
        resp_rx.await.unwrap().unwrap();
        assert!(state.read().await.coils[2]);
    }

    #[tokio::test]
    async fn test_write_multiple_coils() {
        let state = Arc::new(RwLock::new(test_state(0, 6, 0, 0)));
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(reg_worker_loop(Arc::clone(&state), rx));

        let values = vec![true, false, true];
        let (resp_tx, resp_rx) = oneshot::channel();
        tx.send(RegCmd::WriteMultipleCoils {
            addr: 1,
            values,
            resp: resp_tx,
        })
        .unwrap();
        resp_rx.await.unwrap().unwrap();
        let s = state.read().await;
        assert!(!s.coils[0]);
        assert!(s.coils[1]);
        assert!(!s.coils[2]);
        assert!(s.coils[3]);
    }

    #[tokio::test]
    async fn test_coil_write_oob() {
        let state = Arc::new(RwLock::new(test_state(0, 3, 0, 0)));
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(reg_worker_loop(Arc::clone(&state), rx));

        let (resp_tx, resp_rx) = oneshot::channel();
        tx.send(RegCmd::WriteSingleCoil {
            addr: 10,
            value: true,
            resp: resp_tx,
        })
        .unwrap();
        let err = resp_rx.await.unwrap().unwrap_err();
        assert_eq!(err, ExceptionCode::IllegalDataAddress);
    }

    // ─── 读离散输入 ───

    #[tokio::test]
    async fn test_read_discrete_inputs() {
        let state = Arc::new(RwLock::new(test_state(0, 0, 4, 0)));
        {
            let mut s = state.write().await;
            s.discrete[0] = true;
            s.discrete[2] = true;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(reg_worker_loop(Arc::clone(&state), rx));

        let (resp_tx, resp_rx) = oneshot::channel();
        tx.send(RegCmd::ReadDiscreteInputs {
            addr: 0,
            cnt: 3,
            resp: resp_tx,
        })
        .unwrap();
        let result = resp_rx.await.unwrap().unwrap();
        assert_eq!(result, vec![true, false, true]);
    }

    // ─── 读输入寄存器 ───

    #[tokio::test]
    async fn test_read_input_registers() {
        let state = Arc::new(RwLock::new(test_state(0, 0, 0, 5)));
        {
            let mut s = state.write().await;
            s.input_registers[1] = 77;
            s.input_registers[3] = 99;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(reg_worker_loop(Arc::clone(&state), rx));

        let (resp_tx, resp_rx) = oneshot::channel();
        tx.send(RegCmd::ReadInputRegisters {
            addr: 1,
            cnt: 3,
            resp: resp_tx,
        })
        .unwrap();
        let result = resp_rx.await.unwrap().unwrap();
        assert_eq!(result, vec![77, 0, 99]);
    }

    // ─── OOB 边界测试 ───

    #[tokio::test]
    async fn test_all_oob() {
        let state = Arc::new(RwLock::new(test_state(3, 3, 3, 3)));
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(reg_worker_loop(Arc::clone(&state), rx));

        // ReadHolding OOB
        let (rtx, rrx) = oneshot::channel();
        tx.send(RegCmd::ReadHolding {
            addr: 0,
            cnt: 5,
            resp: rtx,
        })
        .unwrap();
        assert_eq!(
            rrx.await.unwrap().unwrap_err(),
            ExceptionCode::IllegalDataAddress
        );

        // ReadCoils OOB
        let (rtx, rrx) = oneshot::channel();
        tx.send(RegCmd::ReadCoils {
            addr: 0,
            cnt: 5,
            resp: rtx,
        })
        .unwrap();
        assert_eq!(
            rrx.await.unwrap().unwrap_err(),
            ExceptionCode::IllegalDataAddress
        );

        // WriteMultipleCoils OOB
        let (rtx, rrx) = oneshot::channel();
        tx.send(RegCmd::WriteMultipleCoils {
            addr: 1,
            values: vec![true; 5],
            resp: rtx,
        })
        .unwrap();
        assert_eq!(
            rrx.await.unwrap().unwrap_err(),
            ExceptionCode::IllegalDataAddress
        );

        // ReadDiscreteInputs OOB
        let (rtx, rrx) = oneshot::channel();
        tx.send(RegCmd::ReadDiscreteInputs {
            addr: 0,
            cnt: 5,
            resp: rtx,
        })
        .unwrap();
        assert_eq!(
            rrx.await.unwrap().unwrap_err(),
            ExceptionCode::IllegalDataAddress
        );

        // ReadInputRegisters OOB
        let (rtx, rrx) = oneshot::channel();
        tx.send(RegCmd::ReadInputRegisters {
            addr: 0,
            cnt: 5,
            resp: rtx,
        })
        .unwrap();
        assert_eq!(
            rrx.await.unwrap().unwrap_err(),
            ExceptionCode::IllegalDataAddress
        );
    }

    // ─── CRC16 测试 ───

    #[test]
    fn test_crc16() {
        // Modbus RTU 示例: 01 03 00 00 00 01 → CRC 0x840A
        let data = [0x01, 0x03, 0x00, 0x00, 0x00, 0x01];
        let crc = calc_crc16(&data);
        assert_eq!(crc, 0x0A84); // 原始 CRC 值（非帧内字节序）
    }

    // ─── Frame bytes ───

    #[test]
    fn test_frame_bytes_read_coils_rtu() {
        let fi = crate::FrameInfo {
            is_tcp: false,
            unit: 1,
            func_code: 0x01,
            func_name: "读线圈".into(),
            addr: 0,
            values: vec![1, 0, 1, 0, 0, 0, 0, 0], // 8 bits → 1 byte: 0b00000101
            is_request: false,
        };
        let bytes = frame_bytes_from_info(&fi);
        assert!(!bytes.is_empty());
        // 第1字节: 从站地址 (01)
        assert_eq!(bytes[0], 0x01);
        // 第2字节: 功能码 (0x01)
        assert_eq!(bytes[1], 0x01);
        // 第3字节: 后续字节数 (1)
        assert_eq!(bytes[2], 0x01);
        // 第4字节: 线圈数据 (bit0=1, bit2=1 → 0b00000101 = 0x05)
        assert_eq!(bytes[3], 0x05);
        // 最后2字节: CRC
        assert!(bytes.len() >= 5);
    }

    #[test]
    fn test_frame_bytes_read_holding_rtu() {
        let fi = crate::FrameInfo {
            is_tcp: false,
            unit: 1,
            func_code: 0x03,
            func_name: "读保持寄存器".into(),
            addr: 0,
            values: vec![0x1234, 0xABCD],
            is_request: false,
        };
        let bytes = frame_bytes_from_info(&fi);
        assert!(!bytes.is_empty());
        assert_eq!(bytes[0], 0x01); // unit
        assert_eq!(bytes[1], 0x03); // func
        assert_eq!(bytes[2], 4); // byte count (2 regs × 2 bytes)
        assert_eq!(bytes[3], 0x12);
        assert_eq!(bytes[4], 0x34);
        assert_eq!(bytes[5], 0xAB);
        assert_eq!(bytes[6], 0xCD);
        // CRC present
        assert!(bytes.len() >= 8);
    }

    #[test]
    fn test_frame_bytes_write_single_coil_rtu() {
        let fi = crate::FrameInfo {
            is_tcp: false,
            unit: 1,
            func_code: 0x05,
            func_name: "写单线圈".into(),
            addr: 5,
            values: vec![0xFF00],
            is_request: false,
        };
        let bytes = frame_bytes_from_info(&fi);
        assert!(!bytes.is_empty());
        assert_eq!(bytes[0], 0x01); // unit
        assert_eq!(bytes[1], 0x05); // func
        assert_eq!(bytes[2], 0x00); // addr high
        assert_eq!(bytes[3], 0x05); // addr low
        assert_eq!(bytes[4], 0xFF); // value high
        assert_eq!(bytes[5], 0x00); // value low
    }

    #[test]
    fn test_frame_bytes_write_multiple_coils_rtu() {
        let fi = crate::FrameInfo {
            is_tcp: false,
            unit: 1,
            func_code: 0x0F,
            func_name: "写多线圈".into(),
            addr: 10,
            values: vec![1, 0, 1], // 3 coils
            is_request: false,
        };
        let bytes = frame_bytes_from_info(&fi);
        assert!(!bytes.is_empty());
        assert_eq!(bytes[0], 0x01); // unit
        assert_eq!(bytes[1], 0x0F); // func
        assert_eq!(bytes[2], 0x00); // addr high
        assert_eq!(bytes[3], 0x0A); // addr low (10)
        assert_eq!(bytes[4], 0x00); // qty high
        assert_eq!(bytes[5], 0x03); // qty low (3)
    }

    #[test]
    fn test_frame_bytes_read_input_rtu() {
        let fi = crate::FrameInfo {
            is_tcp: false,
            unit: 2,
            func_code: 0x04,
            func_name: "读输入寄存器".into(),
            addr: 0,
            values: vec![0xDEAD],
            is_request: false,
        };
        let bytes = frame_bytes_from_info(&fi);
        assert!(!bytes.is_empty());
        assert_eq!(bytes[0], 0x02); // unit
        assert_eq!(bytes[1], 0x04); // func
        assert_eq!(bytes[2], 2); // byte count
        assert_eq!(bytes[3], 0xDE);
        assert_eq!(bytes[4], 0xAD);
    }

    #[test]
    fn test_frame_bytes_read_discrete_rtu() {
        let fi = crate::FrameInfo {
            is_tcp: false,
            unit: 3,
            func_code: 0x02,
            func_name: "读离散输入".into(),
            addr: 0,
            values: vec![0, 1, 0, 0, 0, 0, 0, 0], // 8 bits → bit1=1
            is_request: false,
        };
        let bytes = frame_bytes_from_info(&fi);
        assert!(!bytes.is_empty());
        assert_eq!(bytes[0], 0x03);
        assert_eq!(bytes[1], 0x02);
        assert_eq!(bytes[2], 1); // byte count
        assert_eq!(bytes[3], 0x02); // bit1=1 → 0b00000010
    }

    #[test]
    fn test_frame_bytes_tcp_format() {
        let fi = crate::FrameInfo {
            is_tcp: true,
            unit: 1,
            func_code: 0x03,
            func_name: "Read Holding Registers".into(),
            addr: 0,
            values: vec![0x0001],
            is_request: false,
        };
        let bytes = frame_bytes_from_info(&fi);
        assert!(!bytes.is_empty());
        // TCP MBAP header: [0x00, 0x01, 0x00, 0x00, len(2 bytes), unit]
        assert_eq!(bytes[0], 0x00); // transaction ID high
        assert_eq!(bytes[1], 0x01); // transaction ID low
        assert_eq!(bytes[2], 0x00); // protocol ID high
        assert_eq!(bytes[3], 0x00); // protocol ID low
        assert_eq!(bytes[5], 0x05); // length (unit(1) + func(1) + bytecount(1) + data(2) = 5)
        assert_eq!(bytes[6], 0x01); // unit
        assert_eq!(bytes[7], 0x03); // func
    }

    /// 测试未知功能码返回空 vec
    #[test]
    fn test_frame_bytes_unknown() {
        let fi = crate::FrameInfo {
            is_tcp: false,
            unit: 1,
            func_code: 0x99,
            func_name: "未知".into(),
            addr: 0,
            values: vec![],
            is_request: false,
        };
        let bytes = frame_bytes_from_info(&fi);
        assert!(bytes.is_empty());
    }
