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
    /* Both persistence pages live in that band and therefore survive a      */
    /* firmware update:                                                      */
    /*   0xEC000  identity          (BoardConfig::identity_flash_page)       */
    /*   0xEB000  radio config      (BoardConfig::radio_config_flash_page)   */
    /* Safe app space = the bootloader's window, 0xEA000 - 0x27000 =         */
    /* 0xC3000 (780K). Was 0xC5000 (788K), which promised 8K the bootloader  */
    /* would have refused to write and reached into both pages above.        */
    FLASH : ORIGIN = 0x00027000, LENGTH = 0xC3000

    /*
     * RAM ORIGIN derived empirically via local Cargo [patch] on
     * nrf-softdevice's softdevice.rs:243 — captured wanted_app_ram_base
     * value sd_ble_enable returns for our config (BleGapConfig:
     * periph=1, central=0; default att_mtu=251; default attr_tab_size).
     *
     * Captured value: wanted_app_ram_base = 0x20002CE0
     *                 softdevice_ram      = 11488 bytes (~11.2 KiB)
     *                 board               = RAK4631 (Pocket V2)
     *                 date                = 2026-05-02
     * Margin: +0x400 (1 KiB) → final RAM ORIGIN = 0x200030E0
     *                          (already 32-byte aligned: 0xE0 / 0x20 = 7)
     * SD reservation: 12512 bytes (= 0x30E0)
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
     * Leaves 256K - 12.2K = 243.8K (0x3CF20) for application.
     */
    RAM   : ORIGIN = 0x200030E0, LENGTH = 0x3CF20
}
