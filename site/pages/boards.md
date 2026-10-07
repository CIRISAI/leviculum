+++
title = "Boards"
+++

What runs the LNode firmware. Every line is a summary of the [board catalogue](https://codeberg.org/Lew_Palm/leviculum/src/branch/master/docs/src/firmware/boards.md), which has the detail.

## Running

- **Heltec Mesh Node T114**: nRF52840, SX1262. Verified: LoRa, BLE, USB, display, GNSS.
- **RAK WisMesh Pocket V2 (RAK19026 carrier)**: RAK4630 module (nRF52840, SX1262). Verified: LoRa, BLE, USB, display, GNSS, battery.
- **RAK4631 bare module**: nRF52840, SX1262. Verified: LoRa, BLE, USB.
- **Seeed SenseCAP Solar Node P1-Pro**: XIAO nRF52840, Wio-SX1262. Bring-up: radio and GNSS not yet confirmed.
- **Heltec WiFi LoRa 32 V4**: ESP32-S3, SX1262. Skeleton: boots, no radio.

## Expected to work

Same radio wiring as a verified board, never run by us. Reports welcome.

- RAK4630 module: RAK WisMesh Pocket Mini (RAK19003), RAK WisMesh Repeater / Hub (RAK2560), RAK WisMesh Tap, RAK WisMesh Tag, WisBlock with RAK13800 Ethernet, WisBlock with RAK14000 e-paper, NomadStar Meteor Pro, MonteOps HW1, GAT562 Mesh Trial Tracker, MeshTiny, muzi R1 Neo
- XIAO nRF52840: Seeed XIAO nRF52840 + Wio-SX1262 kit

## Do not flash

Heltec MeshSolar, LILYGO T-Echo, LILYGO T-Echo Plus, Wio Tracker L1 / L1 e-ink.
