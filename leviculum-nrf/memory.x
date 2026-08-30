/* nRF52840 memory layout — Adafruit-style bootloader with SoftDevice S140 v7.3.0 */
/* NOTE: FLASH ORIGIN must match --base in tools/uf2-runner.sh */
/* Bug32-softdevice-spike Day 3: bumped from v6.1.1 to v7.3.0 to match    */
/* nrf-softdevice's supported S140 family. Both Pocket V2 and T114 ship   */
/* with v6.1.1 from factory; the spike installs v7.3.0 once via UF2-mass- */
/* storage flash. Old (v6.1.1) numbers in comments for reference.         */
MEMORY
{
    /* Application starts after SoftDevice S140 v7.3.0 at 0x27000 (156K).    */
    /* (Was v6.1.1 at 0x26000=152K.)                                         */
    /* Heltec reserves 0xED000-0xF4000 (28K) for license/version data        */
    /* (HARD_VERSION_ADDR, HT_LICENSE_ADDR in variant.h). Bootloader at      */
    /* 0xF4000. The bootloader's own USER_FLASH_END is 0xEA000: it declines  */
    /* every block at or above that address, so 0xEA000-0xF4000 survives a   */
    /* UF2 flash untouched (docs/src/concepts/lnode-flashing.md:139-167).    */
    /* All persistence pages live in that band and therefore survive a       */
    /* firmware update:                                                      */
    /*   0xEC000  identity          (BoardConfig::identity_flash_page)       */
    /*   0xEB000  radio config      (BoardConfig::radio_config_flash_page)   */
    /*   0xEA000  telemetry target (+0x000, #236), user-set fixed           */
    /*            position (+0x100) and media profile (+0x200) — layout    */
    /*            in leviculum_nrf::telemetry, whose compile-time           */
    /*            assertion checks the three records do not overlap         */
    /*            (BoardConfig::telemetry_flash_page)                        */
    /* 0xEA000 is USER_FLASH_END itself: the bootloader declines every block  */
    /* AT or above it, so the page is the lowest one still safe from a UF2.  */
    /* Safe app space = the bootloader's window, 0xEA000 - 0x27000 =         */
    /* 0xC3000 (780K). Was 0xC5000 (788K), which promised 8K the bootloader  */
    /* would have refused to write and reached into both pages above.        */
    FLASH : ORIGIN = 0x00027000, LENGTH = 0xC3000

    /*
     * RAM ORIGIN is the SoftDevice's ceiling and, under flip-link, our
     * stack's FLOOR (`_stack_end`). It is sized from what the S140
     * itself says it needs, measured with `src/bin/sd-ram-probe.rs`:
     * `sd_ble_enable` against a deliberately undersized base answers
     * NRF_ERROR_NO_MEM and writes the exact required base back.
     *
     * 2026-05-02, RAK4631 (Pocket V2), the config that shipped then:
     *     wanted_app_ram_base = 0x20002CE0  (11 488 B of SD RAM)
     *     + 0x400 margin      = 0x200030E0  <- the previous ORIGIN
     *
     * 2026-08-29, #255 T2 probe, case "c1" — conn_count 2, periph 1,
     * central 1, att_mtu 256, event_length 24, i.e. the phase-B
     * configuration (phone + one neighbour LNode):
     *     wanted_app_ram_base = 0x20003BA8  (15 272 B of SD RAM)
     *     + 0x400 margin      = 0x20003FA8
     *     rounded up to 32-byte alignment = 0x20003FC0  <- ORIGIN now
     *
     * The 0x400 margin is deliberately kept rather than spent: the
     * previous ORIGIN carried it, and a configuration that outgrows the
     * floor is a board that panics at boot (see `assert_sd_fits_below_
     * the_stack` in src/ble/mod.rs). The alignment round-up preserves
     * the 32-byte property this file has always had; nothing requires
     * more than 8 bytes.
     *
     * Cost, paid HERE in phase A rather than in phase B, so the stack
     * consequence of the central role is measurable before the role
     * exists (#255 phase A / A4): ORIGIN moves up by
     *     0x20003FC0 - 0x200030E0 = 0xEE0 = 3 808 B
     * and the stack region — [_stack_end, __sdata), everything below
     * .data — shrinks by exactly that. Phase B then raises conn_count
     * and central_role_count with no change to this file.
     *
     * Lower bound: S140 v7 reserves the bottom 8 KiB (0x20000000-
     * 0x20001FFF) for MBR + master-init scratch; cannot probe below
     * that via deliberate undersize.
     *
     * Note: 0x20010000 (64 KiB reservation) regresses with a
     * peripheral-register MEMACC fault (info != 0); root cause now
     * believed to be a layout-driven re-trigger of the same
     * SoftDevice-PREGION violation that caused Bug #32 (direct RNG
     * register access from RawHwRng, fixed in commit f093099). Worth
     * re-running 64 KiB with the f093099 build to confirm.
     *
     * Leaves 256K - 15.9K = 240.1K (0x3C040) for application.
     */
    RAM   : ORIGIN = 0x20003FC0, LENGTH = 0x3C040
}
