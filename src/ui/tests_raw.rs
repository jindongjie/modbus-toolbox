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
}
