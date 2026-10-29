# Prompt: Extract Register Map → Modbus Toolbox Config

Usage: paste the prompt below together with the device register map
(Excel / CSV / PDF / manual text) into an AI, and it will return a
paste-ready TOML profile for `config.toml`.

---

You are a Modbus protocol engineer. I will provide a register map from a device
document (Excel, CSV, PDF, or manual text). Extract every register and produce a
ready-to-paste TOML profile for my Modbus toolbox's config.toml, so the program
can show each register at its correct address with its correctly decoded value.

# 1. What to extract
For every register row in the document, capture:
- address exactly as printed
- bank: holding (4xxxx), input (3xxxx), coil (1xxxx), discrete (2xxxx), or
  0x/zero-based — state which convention the doc uses
- name / description
- data type & size (uint16, int16, uint32, float32, ...)
- read/write
- units, scale factor, and enumerated (bit/enum) values if given
- any byte-order / word-order note (AB CD / CDAB / BA DC / DCBA)

# 2. Address conversion (CRITICAL — this program is 0-based per bank)
- holding 40001 -> 0   (i.e. 4xxxx: subtract 40000; 30001 -> 0, 10001 -> 0, 20001 -> 0)
- If the doc is already 0-based, do NOT subtract. State which base you detected.
- Coils and discrete are bit addresses; holding and input are 16-bit register
  addresses (a 32-bit register occupies 2 consecutive addresses).

# 3. Allowed format strings (only these may appear in the output)
u16, i16, u32, i32, u64, i64, u128, i128, f16, f32, f64, hex, bin, ascii
- u32/i32/f32 consume 2 registers; u64/i64/f64 consume 4; u128/i128 consume 8.
- Map doc types to the closest supported one (float -> f32, double -> f64,
  word -> u16, ...). If a type is unsupported (BCD, packed bits, multi-register
  string), fall back to u16/u32/hex and flag it in the summary — never invent
  an interpretation.

# 4. Output — exactly this TOML block (fill in, don't rename fields):

```toml
[PROFILE_NAME]
main_mode = "tcp-client"   # one of: rtu-client, rtu-server, tcp-client, tcp-server, rtu-monitor, tcp-monitor
tcp_port = 502
tcp_host = "127.0.0.1"     # meaningful for tcp-* modes only
unit = 1                   # slave/unit ID from the doc if stated
holding_count = N          # >= (max holding address + registers it needs) + 1
coil_count = N
discrete_count = N
input_count = N            # >= (max input address + registers it needs) + 1
client_tick_ms = 200
ui_tick_ms = 10
device = "/dev/ttyUSB0"    # rtu-* modes only
baudrate = 9600
databits = 8
parity = "n"
stopbits = 1
flow = "none"
reg_format = "u16"         # default interpretation for every register without a combination

[PROFILE_NAME.labels]
0 = "Name (unit)"          # HOLDING registers only, key = 0-based register index
                           # (no labels exist for input registers — put input
                           # register names in a # comment next to the entry)

[PROFILE_NAME.reg_combinations]
4 = "f32"                  # key = start address, value = format from step 3
8 = "f64"                  # 16-bit entries are allowed too, e.g. 10 = "i16",
                           # to override the base reg_format at one address
i:2 = "f32"                # "i:" prefix = INPUT register bank

[PROFILE_NAME.swap_bytes_reg]
4 = "true"                 # ONLY entries needing a non-default byte order;
                           # "false" is the default — omit it. "i:" prefix = input bank.

[PROFILE_NAME.swap_words_reg]
8 = "true"                 # ONLY for multi-register values with word order
                           # reversed (CDAB); "i:" prefix = input bank.
```

# 5. Decode rules
- 16-bit registers need NO combination entry unless the base reg_format differs
  (e.g. mostly-signed device -> reg_format = "i16", or specific u16 overrides).
- Multi-register combinations must not overlap and must not exceed the bank count.
- Byte/word order mapping (default is ABCD, high word first):
    ABCD        -> no entry
    CDAB        -> swap_words_reg = "true"
    BADC        -> swap_bytes_reg = "true"
    DCBA        -> BOTH byte and word swap = "true"
- Labels: include unit and common enum values, e.g. 0 = "Run state (0=stop,1=run)".
- Scale factors: the program shows RAW decoded values — do NOT scale; note the
  scale (e.g. "x0.1") in the label or summary.

# 6. Self-validate before answering
1. Every combination's range [start, start+regs_needed) fits within its bank count.
2. No two combinations overlap in the same bank.
3. All keys are 0-based integers; "i:" only in reg_combinations / swap maps.
4. Every format value is from the step-3 list.
5. No doc register row was silently dropped.

# 7. Answer format
1. Assumptions (address base, byte order, anything ambiguous) — short table
2. The complete TOML profile block, paste-ready
3. Coverage table: doc register -> config address, format, registers consumed, label
4. Flags: unsupported types, guessed fields, and registers whose byte order I
   should confirm against one known live value
