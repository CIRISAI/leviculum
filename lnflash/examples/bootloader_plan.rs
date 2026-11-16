//! Print what a bootloader self-update UF2 will write to a RAK4631 and exit
//! non-zero if it would touch anything [`protected_regions`] names.
//!
//! The one reader `just bootloader-update-rak` asks before it copies the
//! file (docs/src/concepts/ota-stage-1-ble-dfu.md, "Bootloader update"). An
//! example rather than an lnflash flag on purpose: lnflash never writes a
//! bootloader-family image, and a flag would be the first step to it.
//!
//! Usage: `cargo run -p lnflash --example bootloader_plan -- <uf2> <memory.x>`
//!
//! Output, one fact per line, for the recipe and the reader:
//!
//! ```text
//! BL_PLAN version=0.4.4 blocks=136
//! BL_PLAN literal=0x00000..0x00b00 ...
//! BL_PLAN skipped=0x00000..0x00b00
//! BL_PLAN staged=0xe0000..0xe7b00 pages=0xe0000..0xe8000
//! BL_PLAN installed=0xf4000..0xfe000
//! BL_PLAN store_pages_lost=0xe0000..0xe8000 ...
//! BL_PLAN verdict=ok
//! ```

use std::process::ExitCode;

use lnflash::bootloader_update::{plan, protected_regions, regions_from_memory_x, RAK4631_0_4_3};
use lnflash::uf2::Image;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [uf2_path, memory_x_path] = args.as_slice() else {
        eprintln!("usage: bootloader_plan <uf2> <memory.x>");
        return ExitCode::from(2);
    };
    let bytes = match std::fs::read(uf2_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("bootloader_plan: {uf2_path}: {e}");
            return ExitCode::from(2);
        }
    };
    let memory_x = match std::fs::read_to_string(memory_x_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("bootloader_plan: {memory_x_path}: {e}");
            return ExitCode::from(2);
        }
    };
    let image = match Image::parse(&bytes) {
        Ok(i) => i,
        Err(e) => {
            println!("BL_PLAN verdict=refused reason=\"{e}\"");
            return ExitCode::from(1);
        }
    };
    let protected = protected_regions(&memory_x, &RAK4631_0_4_3);
    let plan = match plan(&image, &RAK4631_0_4_3, &protected) {
        Ok(p) => p,
        Err(e) => {
            println!("BL_PLAN verdict=refused reason=\"{e}\"");
            return ExitCode::from(1);
        }
    };
    let join = |r: &[lnflash::bootloader_update::Range]| {
        r.iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join(",")
    };
    println!(
        "BL_PLAN version={} blocks={}",
        plan.version.as_deref().unwrap_or("unknown"),
        image.blocks.len()
    );
    println!("BL_PLAN literal={}", join(&plan.literal));
    println!("BL_PLAN skipped={}", join(&plan.skipped));
    println!(
        "BL_PLAN staged={} pages={}",
        join(&plan.staged),
        join(&plan.staged_pages)
    );
    println!("BL_PLAN installed={}", plan.installed);
    for region in regions_from_memory_x(&memory_x, &["STORE"]) {
        println!(
            "BL_PLAN store_pages_lost={}",
            join(&plan.pages_in(&region.range))
        );
    }
    println!("BL_PLAN verdict=ok");
    ExitCode::SUCCESS
}
