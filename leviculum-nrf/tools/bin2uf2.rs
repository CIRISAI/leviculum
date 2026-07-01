// bin2uf2. Convert a flat binary to UF2 format.
// Zero crate dependencies. Compile with: rustc tools/bin2uf2.rs -o target/bin2uf2 --edition 2021

use std::env;
use std::fs;
use std::process;

const UF2_MAGIC_START0: u32 = 0x0A324655;
const UF2_MAGIC_START1: u32 = 0x9E5D5157;
const UF2_MAGIC_FINAL: u32 = 0x0AB16F30;
const UF2_FLAG_FAMILY: u32 = 0x00002000;
const PAYLOAD_SIZE: usize = 256;
const BLOCK_SIZE: usize = 512;

fn write_u32_le(buf: &mut [u8], offset: usize, val: u32) {
    buf[offset..offset + 4].copy_from_slice(&val.to_le_bytes());
}

fn parse_hex(s: &str) -> Result<u32, String> {
    let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
    u32::from_str_radix(s, 16).map_err(|e| format!("invalid hex '{}': {}", s, e))
}

fn main() {
    let args: Vec<String> = env::args().collect();

    let (mut base, mut family, mut input, mut output) = (None, None, None, None);
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--base" => { i += 1; base = Some(parse_hex(&args[i]).unwrap_or_else(|e| die(&e))); }
            "--family" => { i += 1; family = Some(parse_hex(&args[i]).unwrap_or_else(|e| die(&e))); }
            _ if input.is_none() => input = Some(args[i].clone()),
            _ if output.is_none() => output = Some(args[i].clone()),
            _ => die(&format!("unexpected argument: {}", args[i])),
        }
        i += 1;
    }

    let base = base.unwrap_or_else(|| die("missing --base"));
    let family = family.unwrap_or_else(|| die("missing --family"));
    let input = input.unwrap_or_else(|| die("missing input file"));
    let output = output.unwrap_or_else(|| die("missing output file"));

    let data = fs::read(&input).unwrap_or_else(|e| die(&format!("cannot read '{}': {}", input, e)));
    if data.is_empty() {
        die("input file is empty");
    }

    let num_blocks = (data.len() + PAYLOAD_SIZE - 1) / PAYLOAD_SIZE;
    let mut uf2 = vec![0u8; num_blocks * BLOCK_SIZE];

    for block_no in 0..num_blocks {
        let offset = block_no * PAYLOAD_SIZE;
        let chunk_len = (data.len() - offset).min(PAYLOAD_SIZE);
        let blk = &mut uf2[block_no * BLOCK_SIZE..(block_no + 1) * BLOCK_SIZE];

        write_u32_le(blk, 0, UF2_MAGIC_START0);
        write_u32_le(blk, 4, UF2_MAGIC_START1);
        write_u32_le(blk, 8, UF2_FLAG_FAMILY);
        write_u32_le(blk, 12, base + (offset as u32));
        write_u32_le(blk, 16, PAYLOAD_SIZE as u32);
        write_u32_le(blk, 20, block_no as u32);
        write_u32_le(blk, 24, num_blocks as u32);
        write_u32_le(blk, 28, family);

        blk[32..32 + chunk_len].copy_from_slice(&data[offset..offset + chunk_len]);
        // Remaining payload bytes are already zero (vec![0u8; ...])

        write_u32_le(blk, BLOCK_SIZE - 4, UF2_MAGIC_FINAL);
    }

    fs::write(&output, &uf2).unwrap_or_else(|e| die(&format!("cannot write '{}': {}", output, e)));
    eprintln!("    Converted: {} bytes, {} blocks", data.len(), num_blocks);
}

fn die(msg: &str) -> ! {
    eprintln!("bin2uf2: {}", msg);
    eprintln!("Usage: bin2uf2 --base 0x26000 --family 0xADA52840 input.bin output.uf2");
    process::exit(1);
}
