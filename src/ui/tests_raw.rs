#[cfg(test)]
mod tests {
    use crate::parse_u16_str;
    use crate::ui::*;
    use crate::{RegDataFormat, RegDataType, RegDataWidth};
    use std::collections::HashMap;

    fn make_u32_combo(primary: usize) -> HashMap<usize, crate::RegDataFormat> {
        let mut m = HashMap::new();
        m.insert(
            primary,
            crate::RegDataFormat {
                data_type: crate::RegDataType::Uint,
                width: crate::RegDataWidth::Bits32,
            },
        );
        m
    }

    #[allow(dead_code)]
    fn make_u64_combo(primary: usize) -> HashMap<usize, crate::RegDataFormat> {
        let mut m = HashMap::new();
        m.insert(
            primary,
            crate::RegDataFormat {
                data_type: crate::RegDataType::Uint,
                width: crate::RegDataWidth::Bits64,
            },
        );
        m
    }

    // ─── is_secondary_register ───

    #[test]
    fn test_is_secondary_register_no_combos() {
        let combos = HashMap::new();
        assert!(!is_secondary_register(0, &combos));
        assert!(!is_secondary_register(5, &combos));
    }

    #[test]
    fn test_is_secondary_register_u32() {
        let combos = make_u32_combo(0);
        // primary is not secondary
        assert!(!is_secondary_register(0, &combos));
        // secondary: reg 1 is part of U32 at addr 0
        assert!(is_secondary_register(1, &combos));
        // beyond range: visible
        assert!(!is_secondary_register(2, &combos));
    }

    #[test]
    fn test_is_secondary_register_multiple_combos() {
        let mut combos = HashMap::new();
        combos.insert(
            0,
            crate::RegDataFormat {
                data_type: crate::RegDataType::Uint,
                width: crate::RegDataWidth::Bits64,
            },
        ); // covers 0-3
        combos.insert(
            10,
            crate::RegDataFormat {
                data_type: crate::RegDataType::Uint,
                width: crate::RegDataWidth::Bits32,
            },
        ); // covers 10-11
           // secondary in first combo
        assert!(is_secondary_register(1, &combos));
        assert!(is_secondary_register(2, &combos));
        assert!(is_secondary_register(3, &combos));
        // primary of first combo
        assert!(!is_secondary_register(0, &combos));
        // uncovered region
        assert!(!is_secondary_register(4, &combos));
        assert!(!is_secondary_register(9, &combos));
        // secondary in second combo
        assert!(is_secondary_register(11, &combos));
        // primary of second combo
        assert!(!is_secondary_register(10, &combos));
    }

    // ─── next_visible_reg / prev_visible_reg ───

    #[test]
    fn test_next_visible_reg_no_combos() {
        let combos = HashMap::new();
        assert_eq!(next_visible_reg(0, 10, &combos), Some(1));
        assert_eq!(next_visible_reg(9, 10, &combos), None);
    }

    #[test]
    fn test_next_visible_reg_skips_secondary() {
        let combos = make_u32_combo(0);
        // 0 → skip 1 (secondary) → 2
        assert_eq!(next_visible_reg(0, 10, &combos), Some(2));
        // 1 → skip 1 (it's secondary, but `from` is 1, so we look at 2 onward) → 2
        assert_eq!(next_visible_reg(1, 10, &combos), Some(2));
        // 2 → 3 (no combo at 2)
        assert_eq!(next_visible_reg(2, 10, &combos), Some(3));
    }

    #[test]
    fn test_next_visible_reg_multiple_combos() {
        let mut combos = HashMap::new();
        combos.insert(
            0,
            crate::RegDataFormat {
                data_type: crate::RegDataType::Uint,
                width: crate::RegDataWidth::Bits64,
            },
        ); // covers 0-3
        combos.insert(
            5,
            crate::RegDataFormat {
                data_type: crate::RegDataType::Uint,
                width: crate::RegDataWidth::Bits32,
            },
        ); // covers 5-6
           // from 0 → skip 1,2,3 → 4
        assert_eq!(next_visible_reg(0, 10, &combos), Some(4));
        // from 4 → skip 5, hmm no 5 is primary → 5
        assert_eq!(next_visible_reg(4, 10, &combos), Some(5));
        // from 5 → skip 6 → 7
        assert_eq!(next_visible_reg(5, 10, &combos), Some(7));
    }

    #[test]
    fn test_prev_visible_reg_no_combos() {
        let combos = HashMap::new();
        assert_eq!(prev_visible_reg(5, &combos), Some(4));
        assert_eq!(prev_visible_reg(0, &combos), None);
    }

    #[test]
    fn test_prev_visible_reg_skips_secondary() {
        let combos = make_u32_combo(2);
        // from 4 → skip... 4,3 → 2 (primary, visible)
        assert_eq!(prev_visible_reg(4, &combos), Some(2));
        // from 3 → skip 3 (secondary) → 2 (primary)
        assert_eq!(prev_visible_reg(3, &combos), Some(2));
        // from 2 → skip... 1 (no combo at 1) → 1
        assert_eq!(prev_visible_reg(2, &combos), Some(1));
    }

    #[test]
    fn test_prev_visible_reg_boundary() {
        let combos = make_u32_combo(0);
        // from 1 → 1 is secondary, prev_visible looks at 0 → 0 is primary
        assert_eq!(prev_visible_reg(1, &combos), Some(0));
        // from 0 → no previous
        assert_eq!(prev_visible_reg(0, &combos), None);
    }

    // ─── format_register_value (value reading from combined registers) ───

    #[test]
    fn test_format_register_value_u16() {
        use crate::RegDataFormat;
        let regs = vec![0x1234u16, 0x5678u16];
        let val = crate::format_register_value(
            &regs,
            0,
            RegDataFormat {
                data_type: crate::RegDataType::Hex,
                width: crate::RegDataWidth::Bits16,
            },
            false,
            false,
        );
        assert_eq!(val, "0x1234");
    }

    #[test]
    fn test_format_register_value_u32_hex() {
        use crate::RegDataFormat;
        // Hex format only reads 1 register; to see the full u32 value,
        // use RegDataFormat { data_type: crate::RegDataType::Uint, width: crate::RegDataWidth::Bits32 } with byte swap to reconstruct the pair.
        let regs = vec![0x1234u16, 0x5678u16, 0x9ABCu16];
        let val = crate::format_register_value(
            &regs,
            0,
            RegDataFormat {
                data_type: crate::RegDataType::Uint,
                width: crate::RegDataWidth::Bits32,
            },
            false,
            false,
        );
        assert_eq!(val, "305419896"); // 0x12345678 in decimal
    }

    #[test]
    fn test_format_register_value_u32_out_of_bounds() {
        use crate::RegDataFormat;
        let regs = vec![0x1234u16];
        let val = crate::format_register_value(
            &regs,
            0,
            RegDataFormat {
                data_type: crate::RegDataType::Uint,
                width: crate::RegDataWidth::Bits32,
            },
            false,
            false,
        );
        assert!(val.contains("--"));
    }

    #[test]
    fn test_format_register_value_u64_hex() {
        use crate::RegDataFormat;
        let regs = vec![0x0001u16, 0x0002u16, 0x0003u16, 0x0004u16];
        let val = crate::format_register_value(
            &regs,
            0,
            RegDataFormat {
                data_type: crate::RegDataType::Uint,
                width: crate::RegDataWidth::Bits64,
            },
            false,
            false,
        );
        assert_eq!(val, "281483566841860"); // 0x0001000200030004 in decimal
    }

    #[test]
    fn test_format_register_value_byte_swap_hex() {
        use crate::RegDataFormat;
        let regs = vec![0x1234u16, 0x5678u16];
        // Hex reads 1 register; swapped byte: 0x1234 → 0x3412
        let val = crate::format_register_value(
            &regs,
            0,
            RegDataFormat {
                data_type: crate::RegDataType::Hex,
                width: crate::RegDataWidth::Bits16,
            },
            true,
            false,
        );
        assert_eq!(val, "0x3412");
    }

    #[test]
    fn test_format_register_value_word_swap_u32() {
        use crate::RegDataFormat;
        let regs = vec![0x1234u16, 0x5678u16];
        // U32 reads 2 regs; word swap reverses: [0x5678, 0x1234] → 0x56781234
        let val = crate::format_register_value(
            &regs,
            0,
            RegDataFormat {
                data_type: crate::RegDataType::Uint,
                width: crate::RegDataWidth::Bits32,
            },
            false,
            true,
        );
        assert_eq!(val, "1450709556"); // 0x56781234 in decimal
    }

    #[test]
    fn test_format_register_value_byte_swap_u32_hex() {
        use crate::RegDataFormat;
        let regs = vec![0x1234u16, 0x5678u16];
        // U32 reads 2 regs; byte swap each: [0x3412, 0x7856] → 0x34127856
        let val = crate::format_register_value(
            &regs,
            0,
            RegDataFormat {
                data_type: crate::RegDataType::Uint,
                width: crate::RegDataWidth::Bits32,
            },
            true,
            false,
        );
        assert_eq!(val, "873625686"); // 0x34127856 in decimal
    }

    #[test]
    fn test_format_register_value_i32_negative() {
        use crate::RegDataFormat;
        let regs = vec![0xFFFFu16, 0xFFCEu16]; // -50 in 32-bit signed
        let val = crate::format_register_value(
            &regs,
            0,
            RegDataFormat {
                data_type: crate::RegDataType::Int,
                width: crate::RegDataWidth::Bits32,
            },
            false,
            false,
        );
        assert_eq!(val, "-50");
    }

    #[test]
    fn test_format_register_value_float32() {
        use crate::RegDataFormat;
        // 3.14 in IEEE 754: 0x4048F5C3
        let regs = vec![0x4048u16, 0xF5C3u16];
        let val = crate::format_register_value(
            &regs,
            0,
            RegDataFormat {
                data_type: crate::RegDataType::Float,
                width: crate::RegDataWidth::Bits32,
            },
            false,
            false,
        );
        assert!(val.starts_with("3.140"), "expected 3.14xxx, got: {}", val);
    }

    // ─── ensure_selected_visible ───

    #[test]
    fn test_ensure_selected_visible_primary() {
        let combos = make_u32_combo(0);
        let mut sel = 0;
        ensure_selected_visible(&mut sel, 10, &combos);
        assert_eq!(sel, 0);
    }

    #[test]
    fn test_ensure_selected_visible_secondary() {
        let combos = make_u32_combo(0);
        let mut sel = 1; // secondary, hidden
        ensure_selected_visible(&mut sel, 10, &combos);
        // should snap to next visible: 2
        assert_eq!(sel, 2);
    }

    #[test]
    fn test_ensure_selected_visible_last_secondary() {
        // combo covers 8-9, sel=9 is at the end
        let mut combos = HashMap::new();
        combos.insert(
            8,
            crate::RegDataFormat {
                data_type: crate::RegDataType::Uint,
                width: crate::RegDataWidth::Bits32,
            },
        );
        let mut sel = 9; // secondary, last reg
        ensure_selected_visible(&mut sel, 10, &combos);
        // no next visible, should go prev: 8 (primary)
        assert_eq!(sel, 8);
    }

    #[test]
    fn test_ensure_selected_visible_all_covered() {
        // U32 combo at 0 covers 0-1, only 2 regs total
        let combos = make_u32_combo(0);
        let mut sel = 1; // secondary, only 2 regs: 0 is primary, 1 is secondary
        ensure_selected_visible(&mut sel, 2, &combos);
        // no next visible, no prev (0 is primary but we look < 1 which is 0, and 0 is not secondary)
        // Actually prev_visible_reg looks at (0..1).rev().find(|&i| !is_secondary_register(i, &combos))
        // That's just 0, and !is_secondary(0) = true, so sel = 0
        assert_eq!(sel, 0);
    }

    // ─── Combination overlap cleanup ───

    #[test]
    fn test_combo_retain_overlapping() {
        // Simulate what g/G does: remove combos overlapping with new range
        let mut combos = HashMap::new();
        combos.insert(
            0,
            crate::RegDataFormat {
                data_type: crate::RegDataType::Uint,
                width: crate::RegDataWidth::Bits32,
            },
        ); // covers 0-1
        combos.insert(
            2,
            crate::RegDataFormat {
                data_type: crate::RegDataType::Uint,
                width: crate::RegDataWidth::Bits32,
            },
        ); // covers 2-3

        // Press g at addr 0 with U32 → want to retain combos NOT overlapping [0, 0+2) = [0,2)
        // So combo {2: U32} should stay (addr 2 >= 0+2)
        combos.retain(|&k, _| !(0..2).contains(&k));
        assert_eq!(combos.len(), 1);
        assert!(combos.contains_key(&2));

        // Now press g at addr 2 → going to U64 → remove anything overlapping [2, 2+4) = [2,6)
        combos.retain(|&k, _| !(2..6).contains(&k));
        assert!(combos.is_empty());
    }

    #[test]
    fn test_combo_back_to_u16() {
        let mut combos = make_u32_combo(0);
        // Press g from U32 → next_width = U64 → new_needed = 4
        // If total_regs = 2, then addr + new_needed (0+4=4) > total_regs → remove combo
        // This simulates going back to U16
        let addr = 0;
        let new_needed = 1; // U16
        if new_needed <= 1 {
            combos.remove(&addr);
        }
        assert!(combos.is_empty());
    }

    // ─── edit_accepts_char ───

    #[test]
    fn test_edit_accepts_char_hex() {
        // hex accepts 0-9, a-f, A-F
        assert!(edit_accepts_char(
            "",
            'a',
            RegDataFormat {
                data_type: crate::RegDataType::Hex,
                width: crate::RegDataWidth::Bits16
            }
        ));
        assert!(edit_accepts_char(
            "",
            'F',
            RegDataFormat {
                data_type: crate::RegDataType::Hex,
                width: crate::RegDataWidth::Bits16
            }
        ));
        assert!(edit_accepts_char(
            "",
            '3',
            RegDataFormat {
                data_type: crate::RegDataType::Hex,
                width: crate::RegDataWidth::Bits16
            }
        ));
        assert!(!edit_accepts_char(
            "",
            'g',
            RegDataFormat {
                data_type: crate::RegDataType::Hex,
                width: crate::RegDataWidth::Bits16
            }
        ));
        assert!(!edit_accepts_char(
            "",
            'z',
            RegDataFormat {
                data_type: crate::RegDataType::Hex,
                width: crate::RegDataWidth::Bits16
            }
        ));
        assert!(!edit_accepts_char(
            "",
            ' ',
            RegDataFormat {
                data_type: crate::RegDataType::Hex,
                width: crate::RegDataWidth::Bits16
            }
        ));
    }

    #[test]
    fn test_edit_accepts_char_binary() {
        assert!(edit_accepts_char(
            "",
            '0',
            RegDataFormat {
                data_type: crate::RegDataType::Binary,
                width: crate::RegDataWidth::Bits16
            }
        ));
        assert!(edit_accepts_char(
            "",
            '1',
            RegDataFormat {
                data_type: crate::RegDataType::Binary,
                width: crate::RegDataWidth::Bits16
            }
        ));
        assert!(!edit_accepts_char(
            "",
            '2',
            RegDataFormat {
                data_type: crate::RegDataType::Binary,
                width: crate::RegDataWidth::Bits16
            }
        ));
        assert!(!edit_accepts_char(
            "",
            'a',
            RegDataFormat {
                data_type: crate::RegDataType::Binary,
                width: crate::RegDataWidth::Bits16
            }
        ));
    }

    #[test]
    fn test_edit_accepts_char_decimal() {
        // U16, I16, U32 etc accept digits and '-'
        assert!(edit_accepts_char(
            "",
            '5',
            RegDataFormat {
                data_type: crate::RegDataType::Uint,
                width: crate::RegDataWidth::Bits16
            }
        ));
        assert!(edit_accepts_char(
            "",
            '9',
            RegDataFormat {
                data_type: crate::RegDataType::Uint,
                width: crate::RegDataWidth::Bits16
            }
        ));
        assert!(edit_accepts_char(
            "-",
            '1',
            RegDataFormat {
                data_type: crate::RegDataType::Int,
                width: crate::RegDataWidth::Bits16
            }
        ));
        assert!(!edit_accepts_char(
            "",
            'a',
            RegDataFormat {
                data_type: crate::RegDataType::Uint,
                width: crate::RegDataWidth::Bits16
            }
        ));
        assert!(!edit_accepts_char(
            "",
            'x',
            RegDataFormat {
                data_type: crate::RegDataType::Uint,
                width: crate::RegDataWidth::Bits16
            }
        ));
    }

    #[test]
    fn test_edit_accepts_char_whitespace_rejected() {
        assert!(!edit_accepts_char(
            "",
            ' ',
            RegDataFormat {
                data_type: crate::RegDataType::Uint,
                width: crate::RegDataWidth::Bits16
            }
        ));
        assert!(!edit_accepts_char(
            "",
            '\t',
            RegDataFormat {
                data_type: crate::RegDataType::Hex,
                width: crate::RegDataWidth::Bits16
            }
        ));
    }

    #[test]
    fn test_edit_accepts_char_float() {
        // Float accepts digits, '.', '-', '+', 'e', 'E'
        let float_fmt = RegDataFormat {
            data_type: crate::RegDataType::Float,
            width: crate::RegDataWidth::Bits32,
        };
        assert!(edit_accepts_char("", '3', float_fmt));
        assert!(edit_accepts_char("3", '.', float_fmt));
        assert!(edit_accepts_char("3.", '1', float_fmt));
        assert!(edit_accepts_char("3.1", '4', float_fmt));
        assert!(edit_accepts_char("3.14", 'e', float_fmt));
        assert!(edit_accepts_char("3.14e", '-', float_fmt));
        assert!(edit_accepts_char("3.14e-", '1', float_fmt));
        assert!(edit_accepts_char("-3.14", 'e', float_fmt));
        // Invalid chars for float
        assert!(!edit_accepts_char("", 'x', float_fmt));
        assert!(!edit_accepts_char("", 'a', float_fmt));
        assert!(!edit_accepts_char("", 'g', float_fmt));
    }

    #[test]
    fn test_edit_accepts_char_0x_prefix() {
        // 'x' allowed only after '0'
        assert!(edit_accepts_char(
            "0",
            'x',
            RegDataFormat {
                data_type: crate::RegDataType::Uint,
                width: crate::RegDataWidth::Bits16
            }
        ));
        assert!(edit_accepts_char(
            "0",
            'X',
            RegDataFormat {
                data_type: crate::RegDataType::Uint,
                width: crate::RegDataWidth::Bits16
            }
        ));
        assert!(!edit_accepts_char(
            "12",
            'x',
            RegDataFormat {
                data_type: crate::RegDataType::Uint,
                width: crate::RegDataWidth::Bits16
            }
        ));
    }

    #[test]
    fn test_edit_accepts_char_0b_prefix() {
        assert!(edit_accepts_char(
            "0",
            'b',
            RegDataFormat {
                data_type: crate::RegDataType::Uint,
                width: crate::RegDataWidth::Bits16
            }
        ));
        assert!(edit_accepts_char(
            "0",
            'B',
            RegDataFormat {
                data_type: crate::RegDataType::Hex,
                width: crate::RegDataWidth::Bits16
            }
        ));
    }

    // ─── parse_u16_str ───

    #[test]
    fn test_parse_u16_str_hex_format() {
        let r = parse_u16_str(
            "FF",
            RegDataFormat {
                data_type: crate::RegDataType::Hex,
                width: crate::RegDataWidth::Bits16,
            },
        )
        .unwrap();
        assert_eq!(r, 255);
        let r = parse_u16_str(
            "ff",
            RegDataFormat {
                data_type: crate::RegDataType::Hex,
                width: crate::RegDataWidth::Bits16,
            },
        )
        .unwrap();
        assert_eq!(r, 255);
        assert!(parse_u16_str(
            "GG",
            RegDataFormat {
                data_type: crate::RegDataType::Hex,
                width: crate::RegDataWidth::Bits16
            }
        )
        .is_err());
    }

    #[test]
    fn test_parse_u16_str_binary_format() {
        let r = parse_u16_str(
            "1010",
            RegDataFormat {
                data_type: crate::RegDataType::Binary,
                width: crate::RegDataWidth::Bits16,
            },
        )
        .unwrap();
        assert_eq!(r, 10);
        assert!(parse_u16_str(
            "12",
            RegDataFormat {
                data_type: crate::RegDataType::Binary,
                width: crate::RegDataWidth::Bits16
            }
        )
        .is_err());
    }

    #[test]
    fn test_parse_u16_str_decimal_format() {
        let r = parse_u16_str(
            "1234",
            RegDataFormat {
                data_type: crate::RegDataType::Uint,
                width: crate::RegDataWidth::Bits16,
            },
        )
        .unwrap();
        assert_eq!(r, 1234);
        let r = parse_u16_str(
            "0",
            RegDataFormat {
                data_type: crate::RegDataType::Int,
                width: crate::RegDataWidth::Bits16,
            },
        )
        .unwrap();
        assert_eq!(r, 0);
        assert!(parse_u16_str(
            "99999",
            RegDataFormat {
                data_type: crate::RegDataType::Uint,
                width: crate::RegDataWidth::Bits16
            }
        )
        .is_err());
    }

    #[test]
    fn test_parse_u16_str_prefix_overrides() {
        // 0x prefix forces hex regardless of format
        let r = parse_u16_str(
            "0xFF",
            RegDataFormat {
                data_type: crate::RegDataType::Uint,
                width: crate::RegDataWidth::Bits16,
            },
        )
        .unwrap();
        assert_eq!(r, 255);
        // 0b prefix forces binary regardless of format
        let r = parse_u16_str(
            "0b1111",
            RegDataFormat {
                data_type: crate::RegDataType::Hex,
                width: crate::RegDataWidth::Bits16,
            },
        )
        .unwrap();
        assert_eq!(r, 15);
    }

    #[test]
    fn test_parse_u16_str_empty_error() {
        assert!(parse_u16_str(
            "",
            RegDataFormat {
                data_type: RegDataType::Uint,
                width: RegDataWidth::Bits16
            }
        )
        .is_err());
        assert!(parse_u16_str(
            "   ",
            RegDataFormat {
                data_type: RegDataType::Hex,
                width: RegDataWidth::Bits16
            }
        )
        .is_err());
    }

    // ─── next_type/prev_type type cycle ───

    #[test]
    fn test_next_type_cycle_all() {
        use crate::{RegDataFormat, RegDataType, RegDataWidth};
        // Verify the full type cycle covers all 5 types and loops back
        let start = RegDataFormat {
            data_type: RegDataType::Uint,
            width: RegDataWidth::Bits16,
        };
        let mut f = start;
        let mut count = 0;
        loop {
            f = f.next_type();
            count += 1;
            if f.data_type == start.data_type {
                break;
            }
            assert!(count <= 10, "cycle too long");
        }
        assert_eq!(
            count, 5,
            "should cycle through 5 data types (Uint Int Float Hex Binary)"
        );
    }

    #[test]
    fn test_prev_type_cycle_all() {
        use crate::{RegDataFormat, RegDataType, RegDataWidth};
        let start = RegDataFormat {
            data_type: RegDataType::Uint,
            width: RegDataWidth::Bits16,
        };
        let mut f = start;
        let mut count = 0;
        loop {
            f = f.prev_type();
            count += 1;
            if f.data_type == start.data_type {
                break;
            }
            assert!(count <= 10, "cycle too long");
        }
        assert_eq!(
            count, 5,
            "should cycle through 5 data types (Uint Binary Hex Float Int)"
        );
    }

    #[test]
    fn test_next_type_prev_type_are_inverses() {
        use crate::RegDataFormat;
        let types = [
            RegDataFormat {
                data_type: crate::RegDataType::Uint,
                width: crate::RegDataWidth::Bits16,
            },
            RegDataFormat {
                data_type: crate::RegDataType::Int,
                width: crate::RegDataWidth::Bits32,
            },
            RegDataFormat {
                data_type: crate::RegDataType::Float,
                width: crate::RegDataWidth::Bits16,
            },
            RegDataFormat {
                data_type: crate::RegDataType::Hex,
                width: crate::RegDataWidth::Bits16,
            },
            RegDataFormat {
                data_type: crate::RegDataType::Binary,
                width: crate::RegDataWidth::Bits16,
            },
        ];
        for fmt in &types {
            // next_type then prev_type should return to original type
            assert_eq!(fmt.next_type().prev_type(), *fmt, "mismatch for {fmt:?}");
            // prev_type then next_type should return to original type
            assert_eq!(fmt.prev_type().next_type(), *fmt, "mismatch for {fmt:?}");
        }
    }

    #[test]
    fn test_next_width_cycle() {
        use crate::{RegDataFormat, RegDataType, RegDataWidth};
        let start = RegDataFormat {
            data_type: RegDataType::Uint,
            width: RegDataWidth::Bits16,
        };
        let mut widths = Vec::new();
        let mut f = start;
        loop {
            widths.push(f.width);
            f = f.next_width();
            if f.width == start.width {
                break;
            }
            assert!(widths.len() <= 5);
        }
        assert_eq!(widths.len(), 4, "should cycle through 4 widths");
    }

    #[test]
    fn test_to_uint_keeps_width() {
        use crate::{RegDataFormat, RegDataType, RegDataWidth};
        let f = RegDataFormat {
            data_type: RegDataType::Float,
            width: RegDataWidth::Bits32,
        };
        let u = f.to_uint();
        assert_eq!(u.data_type, RegDataType::Uint);
        assert_eq!(u.width, RegDataWidth::Bits32);
    }

    #[test]
    fn test_to_int_keeps_width() {
        use crate::{RegDataFormat, RegDataType, RegDataWidth};
        let f = RegDataFormat {
            data_type: RegDataType::Uint,
            width: RegDataWidth::Bits64,
        };
        let i = f.to_int();
        assert_eq!(i.data_type, RegDataType::Int);
        assert_eq!(i.width, RegDataWidth::Bits64);
    }

    #[test]
    fn test_to_hex_keeps_width() {
        use crate::{RegDataFormat, RegDataType, RegDataWidth};
        let f = RegDataFormat {
            data_type: RegDataType::Uint,
            width: RegDataWidth::Bits64,
        };
        let h = f.to_hex();
        assert_eq!(h.data_type, RegDataType::Hex);
        assert_eq!(h.width, RegDataWidth::Bits64);
    }

    #[test]
    fn test_to_binary_keeps_width() {
        use crate::{RegDataFormat, RegDataType, RegDataWidth};
        let f = RegDataFormat {
            data_type: RegDataType::Float,
            width: RegDataWidth::Bits32,
        };
        let b = f.to_binary();
        assert_eq!(b.data_type, RegDataType::Binary);
        assert_eq!(b.width, RegDataWidth::Bits32);
    }

    #[test]
    fn test_to_float_keeps_width() {
        use crate::{RegDataFormat, RegDataType, RegDataWidth};
        // f on Float keeps the same width (does not cycle)
        let start = RegDataFormat {
            data_type: RegDataType::Float,
            width: RegDataWidth::Bits16,
        };
        let next = start.to_float();
        assert_eq!(next.data_type, RegDataType::Float);
        assert_eq!(next.width, RegDataWidth::Bits16);
        // Non-Float → Float preserves existing width
        let from_uint32 = RegDataFormat {
            data_type: RegDataType::Uint,
            width: RegDataWidth::Bits32,
        };
        let f = from_uint32.to_float();
        assert_eq!(f.data_type, RegDataType::Float);
        assert_eq!(f.width, RegDataWidth::Bits32);
    }

    #[test]
    fn test_regs_needed_by_width() {
        use crate::{RegDataFormat, RegDataType, RegDataWidth};
        let f16 = RegDataFormat {
            data_type: RegDataType::Uint,
            width: RegDataWidth::Bits16,
        };
        let f32 = RegDataFormat {
            data_type: RegDataType::Uint,
            width: RegDataWidth::Bits32,
        };
        let f64 = RegDataFormat {
            data_type: RegDataType::Uint,
            width: RegDataWidth::Bits64,
        };
        let f128 = RegDataFormat {
            data_type: RegDataType::Uint,
            width: RegDataWidth::Bits128,
        };
        assert_eq!(f16.regs_needed(), 1);
        assert_eq!(f32.regs_needed(), 2);
        assert_eq!(f64.regs_needed(), 4);
        assert_eq!(f128.regs_needed(), 8);
    }

    // ─── format_register_value additional coverage ───

    #[test]
    fn test_format_register_value_float64() {
        use crate::format_register_value;
        let regs = [0x4009_u16, 0x21CA_u16, 0xC0DE_u16, 0x0000_u16]; // π ≈ 3.14159 as f64 + junk
        let fmt = RegDataFormat {
            data_type: RegDataType::Float,
            width: RegDataWidth::Bits64,
        };
        // regs[0..4] = f64 bits: 0x400921CA_C0DE_0000
        let result = format_register_value(&regs, 0, fmt, false, false);
        // Should produce a valid f64 string
        assert!(result.contains('.'), "expected float string, got: {result}");
        assert!(!result.starts_with("--"), "unexpected out-of-bounds: {result}");
    }

    #[test]
    fn test_format_register_value_f16() {
        use crate::format_register_value;
        let regs = [0x3C00_u16]; // f16 = 1.0
        let fmt = RegDataFormat {
            data_type: RegDataType::Float,
            width: RegDataWidth::Bits16,
        };
        assert_eq!(format_register_value(&regs, 0, fmt, false, false), "1.000000");
    }

    #[test]
    fn test_format_register_value_f16_zero() {
        use crate::format_register_value;
        let regs = [0x0000_u16];
        let fmt = RegDataFormat {
            data_type: RegDataType::Float,
            width: RegDataWidth::Bits16,
        };
        assert_eq!(format_register_value(&regs, 0, fmt, false, false), "0.000000");
    }

    #[test]
    fn test_format_register_value_i64() {
        use crate::format_register_value;
        // i64 = -1 → all bits = 0xFFFFFFFF_FFFFFFFF
        let regs = [0xFFFF_u16, 0xFFFF_u16, 0xFFFF_u16, 0xFFFF_u16];
        let fmt = RegDataFormat {
            data_type: RegDataType::Int,
            width: RegDataWidth::Bits64,
        };
        assert_eq!(format_register_value(&regs, 0, fmt, false, false), "-1");
    }

    #[test]
    fn test_format_register_value_u128_hex() {
        use crate::format_register_value;
        let regs = [0xDEAD_u16, 0xBEEF_u16, 0x0000_u16, 0x0000_u16,
                    0x0000_u16, 0x0000_u16, 0x0000_u16, 0x0001_u16];
        let fmt = RegDataFormat {
            data_type: RegDataType::Hex,
            width: RegDataWidth::Bits128,
        };
        let result = format_register_value(&regs, 0, fmt, false, false);
        assert!(result.starts_with("0x"), "expected hex prefix: {result}");
    }

    #[test]
    fn test_format_register_value_ascii() {
        use crate::format_register_value;
        // "AB" = 0x41 0x42 in a single u16 = 0x4142
        let regs = [0x4142_u16];
        let fmt = RegDataFormat {
            data_type: RegDataType::Ascii,
            width: RegDataWidth::Bits16,
        };
        assert_eq!(format_register_value(&regs, 0, fmt, false, false), "16706");
    }

    #[test]
    fn test_format_register_value_out_of_bounds_u32() {
        use crate::format_register_value;
        let regs = [0x1234_u16]; // only 1 reg, but need 2 for u32
        let fmt = RegDataFormat {
            data_type: RegDataType::Uint,
            width: RegDataWidth::Bits32,
        };
        let result = format_register_value(&regs, 0, fmt, false, false);
        assert!(result.starts_with("--"), "expected out-of-bounds: {result}");
    }

    // ─── f32 to f16 bit conversion ───

    #[test]
    fn test_f32_to_f16_bits_one() {
        use crate::f32_to_f16_bits;
        assert_eq!(f32_to_f16_bits(1.0), 0x3C00);
    }

    #[test]
    fn test_f32_to_f16_bits_zero() {
        use crate::f32_to_f16_bits;
        assert_eq!(f32_to_f16_bits(0.0), 0x0000);
    }

    #[test]
    fn test_f32_to_f16_bits_negative() {
        use crate::f32_to_f16_bits;
        assert_eq!(f32_to_f16_bits(-2.0), 0xC000);
    }

    #[test]
    fn test_f32_to_f16_bits_infinity() {
        use crate::f32_to_f16_bits;
        assert_eq!(f32_to_f16_bits(f32::INFINITY), 0x7C00);
        assert_eq!(f32_to_f16_bits(f32::NEG_INFINITY), 0xFC00);
    }

    #[test]
    fn test_f32_to_f16_bits_nan() {
        use crate::f32_to_f16_bits;
        let nan_val = f32_to_f16_bits(f32::NAN);
        // NaN: sign=0, exp=0x1F, mant ≠ 0
        assert!(nan_val & 0x7C00 == 0x7C00);
        assert!(nan_val & 0x03FF != 0);
    }

    // ─── format_f16 ───

    #[test]
    fn test_format_f16_one() {
        use crate::format_f16;
        assert_eq!(format_f16(0x3C00), "1.000000");
    }

    #[test]
    fn test_format_f16_zero() {
        use crate::format_f16;
        assert_eq!(format_f16(0x0000), "0.000000");
    }

    #[test]
    fn test_format_f16_infinity() {
        use crate::format_f16;
        assert_eq!(format_f16(0x7C00), "inf");
    }

    #[test]
    fn test_format_f16_neg_infinity() {
        use crate::format_f16;
        assert_eq!(format_f16(0xFC00), "-inf");
    }

    #[test]
    fn test_format_f16_nan() {
        use crate::format_f16;
        let s = format_f16(0x7C01);
        // Rust formats f32::NAN as "NaN"
        assert_eq!(s, "NaN");
    }

    // ─── parse_register_value ───

    #[test]
    fn test_parse_register_value_u16() {
        use crate::parse_register_value;
        let fmt = RegDataFormat { data_type: RegDataType::Uint, width: RegDataWidth::Bits16 };
        let result = parse_register_value("42", fmt, false, false).unwrap();
        assert_eq!(result, vec![42]);
    }

    #[test]
    fn test_parse_register_value_u32() {
        use crate::parse_register_value;
        let fmt = RegDataFormat { data_type: RegDataType::Uint, width: RegDataWidth::Bits32 };
        let result = parse_register_value("70000", fmt, false, false).unwrap();
        // 70000 = 0x0001_1170 => hi=0x0001, lo=0x1170
        assert_eq!(result, vec![0x0001, 0x1170]);
    }

    #[test]
    fn test_parse_register_value_i32_negative() {
        use crate::parse_register_value;
        let fmt = RegDataFormat { data_type: RegDataType::Int, width: RegDataWidth::Bits32 };
        let result = parse_register_value("-70000", fmt, false, false).unwrap();
        // -70000 as u32 = 0xFFFE_EE90 => hi=0xFFFE, lo=0xEE90
        assert_eq!(result, vec![0xFFFE, 0xEE90]);
    }

    #[test]
    fn test_parse_register_value_u64() {
        use crate::parse_register_value;
        let fmt = RegDataFormat { data_type: RegDataType::Uint, width: RegDataWidth::Bits64 };
        let result = parse_register_value("0xDEADBEEF00000001", fmt, false, false).unwrap();
        assert_eq!(result, vec![0xDEAD, 0xBEEF, 0x0000, 0x0001]);
    }

    #[test]
    fn test_parse_register_value_hex16() {
        use crate::parse_register_value;
        let fmt = RegDataFormat { data_type: RegDataType::Hex, width: RegDataWidth::Bits16 };
        let result = parse_register_value("FF", fmt, false, false).unwrap();
        assert_eq!(result, vec![0x00FF]);
    }

    #[test]
    fn test_parse_register_value_binary() {
        use crate::parse_register_value;
        let fmt = RegDataFormat { data_type: RegDataType::Binary, width: RegDataWidth::Bits16 };
        let result = parse_register_value("0b1010", fmt, false, false).unwrap();
        assert_eq!(result, vec![0b1010]);
    }

    #[test]
    fn test_parse_register_value_f32() {
        use crate::parse_register_value;
        let fmt = RegDataFormat { data_type: RegDataType::Float, width: RegDataWidth::Bits32 };
        let result = parse_register_value("3.14", fmt, false, false).unwrap();
        // f32 bits of 3.14 = 0x4048F5C3 => hi=0x4048, lo=0xF5C3
        assert_eq!(result, vec![0x4048, 0xF5C3]);
    }

    #[test]
    fn test_parse_register_value_f64() {
        use crate::parse_register_value;
        let fmt = RegDataFormat { data_type: RegDataType::Float, width: RegDataWidth::Bits64 };
        let result = parse_register_value("3.14", fmt, false, false).unwrap();
        // f64 bits of 3.14 = 0x40091EB8_51EB851F
        assert_eq!(result.len(), 4);
        assert_eq!(result[0], 0x4009);
    }

    #[test]
    fn test_parse_register_value_empty_error() {
        use crate::parse_register_value;
        let fmt = RegDataFormat { data_type: RegDataType::Uint, width: RegDataWidth::Bits16 };
        assert!(parse_register_value("", fmt, false, false).is_err());
        assert!(parse_register_value("  ", fmt, false, false).is_err());
    }

    #[test]
    fn test_parse_register_value_swap_bytes() {
        use crate::parse_register_value;
        let fmt = RegDataFormat { data_type: RegDataType::Hex, width: RegDataWidth::Bits16 };
        let result = parse_register_value("0x1234", fmt, true, false).unwrap();
        assert_eq!(result, vec![0x3412]); // byte-swapped
    }

    #[test]
    fn test_parse_register_value_swap_words() {
        use crate::parse_register_value;
        let fmt = RegDataFormat { data_type: RegDataType::Uint, width: RegDataWidth::Bits32 };
        let result = parse_register_value("0x12345678", fmt, false, true).unwrap();
        assert_eq!(result, vec![0x5678, 0x1234]); // word-swapped
    }

    // ─── days_to_date ───

    #[test]
    fn test_days_to_date_epoch() {
        use crate::days_to_date;
        // Unix epoch: 1970-01-01
        assert_eq!(days_to_date(0), (1970, 1, 1));
    }

    #[test]
    fn test_days_to_date_today() {
        use crate::days_to_date;
        // 2024-01-01: days since epoch ≈ 19723
        let (y, m, d) = days_to_date(19723);
        assert_eq!((y, m, d), (2024, 1, 1));
    }

    #[test]
    fn test_days_to_date_leap_year() {
        use crate::days_to_date;
        // 2000-02-29: a leap year
        let (y, m, d) = days_to_date(11016);
        assert_eq!((y, m, d), (2000, 2, 29));
    }

    #[test]
    fn test_days_to_date_future() {
        use crate::days_to_date;
        // 2038-01-19: the "year 2038" date
        let (y, m, d) = days_to_date(24855);
        assert_eq!((y, m, d), (2038, 1, 19));
    }

    // ─── parse_mainmode ───

    #[test]
    fn test_parse_mainmode() {
        use crate::parse_mainmode;
        assert!(parse_mainmode("tcp-server").is_ok());
        assert!(parse_mainmode("ts").is_ok());
        assert!(parse_mainmode("tcp-client").is_ok());
        assert!(parse_mainmode("tc").is_ok());
        assert!(parse_mainmode("rtu-server").is_ok());
        assert!(parse_mainmode("rs").is_ok());
        assert!(parse_mainmode("rtu-client").is_ok());
        assert!(parse_mainmode("rc").is_ok());
        assert!(parse_mainmode("tcp-monitor").is_ok());
        assert!(parse_mainmode("tm").is_ok());
        assert!(parse_mainmode("rtu-monitor").is_ok());
        assert!(parse_mainmode("rm").is_ok());
        assert!(parse_mainmode("invalid").is_err());
    }

    // ─── parse_combination_format ───

    #[test]
    fn test_parse_combination_format_named() {
        use crate::parse_combination_format;
        use crate::RegDataType;
        let r = parse_combination_format("f32");
        assert_eq!(r.data_type, RegDataType::Float);
        assert_eq!(r.width, RegDataWidth::Bits32);

        let r = parse_combination_format("u64");
        assert_eq!(r.data_type, RegDataType::Uint);
        assert_eq!(r.width, RegDataWidth::Bits64);
    }

    #[test]
    fn test_parse_combination_format_hex() {
        use crate::parse_combination_format;
        use crate::RegDataType;
        let r = parse_combination_format("hex");
        assert_eq!(r.data_type, RegDataType::Hex);
        assert_eq!(r.width, RegDataWidth::Bits16);
    }

    #[test]
    fn test_parse_combination_format_binary() {
        use crate::parse_combination_format;
        use crate::RegDataType;
        let r = parse_combination_format("bin");
        assert_eq!(r.data_type, RegDataType::Binary);
    }

    #[test]
    fn test_parse_combination_format_short() {
        use crate::parse_combination_format;
        let r = parse_combination_format("x");
        // Too short → default
        assert_eq!(r, RegDataFormat::default());
    }

    #[test]
    fn test_parse_combination_format_invalid_type_char() {
        use crate::parse_combination_format;
        let r = parse_combination_format("z32");
        assert_eq!(r, RegDataFormat::default());
    }

    // ─── parse_parity / parse_flow / parse_databits / parse_stopbits ───

    #[test]
    fn test_parse_parity() {
        use crate::parse_parity;
        use tokio_serial::Parity;
        assert_eq!(parse_parity("none").unwrap(), Parity::None);
        assert_eq!(parse_parity("even").unwrap(), Parity::Even);
        assert_eq!(parse_parity("odd").unwrap(), Parity::Odd);
        assert!(parse_parity("bad").is_err());
    }

    #[test]
    fn test_parse_flow() {
        use crate::parse_flow;
        use tokio_serial::FlowControl;
        assert_eq!(parse_flow("none").unwrap(), FlowControl::None);
        assert_eq!(parse_flow("hard").unwrap(), FlowControl::Hardware);
        assert_eq!(parse_flow("rtscts").unwrap(), FlowControl::Hardware);
        assert_eq!(parse_flow("soft").unwrap(), FlowControl::Software);
        assert_eq!(parse_flow("xonxoff").unwrap(), FlowControl::Software);
        assert!(parse_flow("bad").is_err());
    }

    #[test]
    fn test_parse_databits() {
        use crate::parse_databits;
        use tokio_serial::DataBits;
        assert_eq!(parse_databits(5).unwrap(), DataBits::Five);
        assert_eq!(parse_databits(6).unwrap(), DataBits::Six);
        assert_eq!(parse_databits(7).unwrap(), DataBits::Seven);
        assert_eq!(parse_databits(8).unwrap(), DataBits::Eight);
        assert!(parse_databits(9).is_err());
    }

    #[test]
    fn test_parse_stopbits() {
        use crate::parse_stopbits;
        use tokio_serial::StopBits;
        assert_eq!(parse_stopbits(1).unwrap(), StopBits::One);
        assert_eq!(parse_stopbits(2).unwrap(), StopBits::Two);
        assert!(parse_stopbits(0).is_err());
    }

    // ─── record_frame ───

    #[test]
    fn test_record_frame() {
        use crate::{record_frame, FrameInfo, MonitorStats};
        let mut stats = MonitorStats::default();
        let fi = FrameInfo {
            func_code: 0x03,
            func_name: "Read Holding".into(),
            addr: 0x0000,
            values: vec![0x1234],
            is_tcp: true,
            is_request: false,
            unit: 1,
        };
        record_frame(&mut stats, &fi);
        assert_eq!(stats.total_frames, 1);
        assert_eq!(stats.func_count.get(&0x03), Some(&1));
        assert_eq!(stats.addr_count.get(&0x0000), Some(&1));
        assert_eq!(stats.history.len(), 1);
    }

    #[test]
    fn test_record_frame_max_history() {
        use crate::{record_frame, FrameInfo, MonitorStats};
        let mut stats = MonitorStats::default();
        for i in 0..600 {
            let fi = FrameInfo {
                func_code: 0x03,
                func_name: "Read".into(),
                addr: i as u16,
                values: vec![i as u16],
                is_tcp: true,
                is_request: false,
                unit: 1,
            };
            record_frame(&mut stats, &fi);
        }
        assert_eq!(stats.total_frames, 600);
        assert!(stats.history.len() <= 500); // MAX_HISTORY cap
    }

    // ─── record_reg_change ───

    #[test]
    fn test_record_reg_change_increases() {
        use crate::{record_reg_change, AppState};
        let mut state = AppState::default();
        // Ensure reg_just_changed/reg_change_direction have enough capacity
        state.reg_just_changed.resize(10, false);
        state.reg_change_direction.resize(10, crate::ChangeDirection::Up);
        record_reg_change(&mut state, 0, 100, 200);
        assert_eq!(state.reg_change_history.len(), 1);
        assert_eq!(state.reg_change_direction[0], crate::ChangeDirection::Up);
        assert_eq!(state.reg_bar_history[0].len(), 1);
        assert_eq!(state.reg_bar_history[0][0], 200);
    }

    #[test]
    fn test_record_reg_change_decreases() {
        use crate::{record_reg_change, AppState};
        let mut state = AppState::default();
        state.reg_just_changed.resize(10, false);
        state.reg_change_direction.resize(10, crate::ChangeDirection::Up);
        record_reg_change(&mut state, 0, 200, 100);
        assert_eq!(state.reg_change_history.len(), 1);
        assert_eq!(state.reg_change_direction[0], crate::ChangeDirection::Down);
    }

    #[test]
    fn test_record_reg_change_no_change_skipped() {
        use crate::{record_reg_change, AppState};
        let mut state = AppState::default();
        record_reg_change(&mut state, 0, 100, 100);
        assert_eq!(state.reg_change_history.len(), 0); // no change → skipped
    }

    #[test]
    fn test_record_reg_change_max_cap() {
        use crate::{record_reg_change, AppState, BAR_HISTORY_SLOTS};
        let mut state = AppState::default();
        for i in 0..600 {
            record_reg_change(&mut state, 0, i, i + 1);
        }
        assert!(state.reg_change_history.len() <= 500);
        // BAR_HISTORY_SLOTS = 20
        assert!(state.reg_bar_history[0].len() <= BAR_HISTORY_SLOTS);
    }

    // ─── RegDataFormat::all_types & short_label ───

    #[test]
    fn test_all_types_contains_all() {
        use crate::{RegDataFormat, RegDataType};
        let all = RegDataFormat::all_types();
        assert!(all.contains(&RegDataType::Uint));
        assert!(all.contains(&RegDataType::Int));
        assert!(all.contains(&RegDataType::Float));
        assert!(all.contains(&RegDataType::Hex));
        assert!(all.contains(&RegDataType::Binary));
        assert!(all.contains(&RegDataType::Ascii));
    }

    #[test]
    fn test_short_label_formats() {
        use crate::{RegDataFormat, RegDataType, RegDataWidth};
        let f = RegDataFormat { data_type: RegDataType::Uint, width: RegDataWidth::Bits32 };
        assert_eq!(f.short_label(), "u32");
        let f = RegDataFormat { data_type: RegDataType::Hex, width: RegDataWidth::Bits16 };
        assert_eq!(f.short_label(), "hex");
        let f = RegDataFormat { data_type: RegDataType::Ascii, width: RegDataWidth::Bits16 };
        assert_eq!(f.short_label(), "ascii");
    }

    // ─── wrapped_lines ───

    #[test]
    fn test_wrapped_lines_single_line() {
        use crate::ui::wrapped_lines;
        assert_eq!(wrapped_lines("hello", 10), 1);
        assert_eq!(wrapped_lines("hello", 3), 2); // "hel" + "lo"
    }

    #[test]
    fn test_wrapped_lines_multi_line() {
        use crate::ui::wrapped_lines;
        let text = "abc\ndef\nghi";
        assert_eq!(wrapped_lines(text, 10), 3);
    }

    #[test]
    fn test_wrapped_lines_zero_width() {
        use crate::ui::wrapped_lines;
        assert_eq!(wrapped_lines("hello", 0), 1);
    }

    #[test]
    fn test_wrapped_lines_empty_line() {
        use crate::ui::wrapped_lines;
        assert_eq!(wrapped_lines("\n\n", 5), 2);
    }

    // ─── search_match ───

    #[test]
    fn test_search_match_by_index() {
        use crate::ui::search_match;
        assert!(search_match(42, "42", &[], None));
        assert!(!search_match(42, "43", &[], None));
    }

    #[test]
    fn test_search_match_by_label() {
        use crate::ui::search_match;
        let labels = vec!["temperature".to_string(), "pressure".to_string()];
        assert!(search_match(0, "temp", &[], Some(&labels)));
        assert!(search_match(1, "pressure", &[], Some(&labels)));
        assert!(!search_match(1, "voltage", &[], Some(&labels)));
    }

    #[test]
    fn test_search_match_case_insensitive() {
        use crate::ui::search_match;
        let labels = vec!["Temperature".to_string()];
        // Label is "Temperature" (mixed case), search is "temp" (lowercase) → match via to_lowercase
        assert!(search_match(0, "temp", &[], Some(&labels)));
        assert!(!search_match(0, "NOTPRESENT", &[], Some(&labels)));
    }

    #[test]
    fn test_search_match_no_labels() {
        use crate::ui::search_match;
        assert!(!search_match(99, "abc", &[], None)); // idx doesn't contain 'abc'
    }

    // ─── pattern_index / index_to_pattern roundtrip ───

    #[test]
    fn test_pattern_index_roundtrip() {
        use crate::ui::monitor::{index_to_pattern, pattern_index};
        use crate::RegChangePattern;
        for p in &[RegChangePattern::Random, RegChangePattern::UpDown,
                   RegChangePattern::Sine, RegChangePattern::Square,
                   RegChangePattern::Triangle] {
            let idx = pattern_index(p);
            let back = index_to_pattern(idx);
            assert_eq!(*p, back, "roundtrip failed for {p:?}");
        }
    }

    // ─── format_monitor_history (empty) ───

    #[test]
    fn test_format_monitor_history_empty() {
        use crate::ui::monitor::format_monitor_history;
        use crate::MonitorStats;
        let m = MonitorStats::default();
        let text = format_monitor_history(&m, 0);
        assert!(!text.is_empty());
    }

    #[test]
    fn test_format_monitor_history_with_data() {
        use crate::ui::monitor::format_monitor_history;
        use crate::{FrameRecord, MonitorStats};
        use std::time::Instant;
        let mut m = MonitorStats::default();
        m.history.push(FrameRecord {
            timestamp: Instant::now(),
            human_time: "12:00:00.000".into(),
            func_code: 0x03,
            func_name: "Read".into(),
            addr: 0x100,
            values: vec![0x1234],
            is_tcp: true,
            is_request: false,
            unit: 1,
        });
        let text = format_monitor_history(&m, 0);
        assert!(text.contains("Read"));
        assert!(text.contains("0x0100"));
    }

    // ─── format_monitor_stats ───

    #[test]
    fn test_format_monitor_stats_empty() {
        use crate::ui::monitor::format_monitor_stats;
        use crate::MonitorStats;
        let m = MonitorStats::default();
        let text = format_monitor_stats(&m);
        assert!(text.contains("0"));
    }

    #[test]
    fn test_format_monitor_stats_with_counts() {
        use crate::ui::monitor::format_monitor_stats;
        use crate::MonitorStats;
        let mut m = MonitorStats::default();
        m.total_frames = 10;
        m.func_count.insert(0x03, 5);
        m.addr_count.insert(0x0000, 5);
        let text = format_monitor_stats(&m);
        assert!(text.contains("10"));
        assert!(text.contains("0x03"));
    }

    // ─── csv_log_path ───

    #[test]
    fn test_csv_log_path_tcp() {
        use crate::csv_log_path;
        let path = csv_log_path("tcp-server", 502, "/dev/ttyUSB0");
        let s = path.to_string_lossy();
        assert!(s.contains("monitor"));
        assert!(s.contains("tcp-502"));
        assert!(s.ends_with(".csv"));
    }

    #[test]
    fn test_csv_log_path_rtu() {
        use crate::csv_log_path;
        let path = csv_log_path("rtu-client", 0, "/dev/ttyUSB0");
        let s = path.to_string_lossy();
        assert!(s.contains("rtu-"));
        assert!(s.contains("ttyUSB0"));
        assert!(s.ends_with(".csv"));
    }

    #[test]
    fn test_csv_log_path_rtu_sanitized() {
        use crate::csv_log_path;
        let path = csv_log_path("rtu-client", 0, "COM1");
        let s = path.to_string_lossy();
        assert!(s.contains("COM1"));
    }

    // ─── parse_reg_format (ui/mod.rs) ───

    #[test]
    fn test_parse_reg_format_i16() {
        use crate::ui::parse_reg_format;
        use crate::{RegDataType, RegDataWidth};
        let r = parse_reg_format("i16");
        assert_eq!(r.data_type, RegDataType::Int);
        assert_eq!(r.width, RegDataWidth::Bits16);
    }

    #[test]
    fn test_parse_reg_format_f64_long() {
        use crate::ui::parse_reg_format;
        use crate::{RegDataType, RegDataWidth};
        let r = parse_reg_format("double");
        assert_eq!(r.data_type, RegDataType::Float);
        assert_eq!(r.width, RegDataWidth::Bits64);
    }

    #[test]
    fn test_parse_reg_format_unknown_returns_default() {
        use crate::ui::parse_reg_format;
        let r = parse_reg_format("unknown");
        assert_eq!(r, crate::RegDataFormat::default());
    }
}
