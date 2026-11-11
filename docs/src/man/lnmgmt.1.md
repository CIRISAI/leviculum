# lnmgmt(1)

## NAME

lnmgmt -- Reticulum remote management of a board

## SYNOPSIS

**lnmgmt** [*options*] **dfu** **-R** *transport_hash* **-i** *identity_file* [**-w** *seconds*]

## DESCRIPTION

**lnmgmt** sends the remote-management verbs that change a node's state. The read-only verb, `/status`, stays in **lnstatus**(1), which is held byte-identical to Python's **rnstatus** and so never learns a verb that changes anything.

It connects to a running daemon (**lnsd** or **rnsd**) via shared instance IPC, requests a path to the node's `rnstransport.remote.management` destination, establishes a link, identifies with the management identity and sends one request, the same flow **lnstatus -R** runs. The node answers only an identity on its allow-list, which on a board is set over the cable with **lnflash --management-identity**.

## VERBS

**dfu**
:   Ask a board to enter its bootloader's BLE DFU mode, so a phone within Bluetooth range can push a new image (OTA stage 1). The board answers with a typed verdict, printed on standard output as one line: `accepted: the board resets into BLE DFU now`, `refused: not listed` (the identity is not on the board's allow-list) or `refused: no identity` (the link was not identified). Only a board serves this verb; a daemon does not, so a `dfu` to **lnsd** or **rnsd** times out. The board then stays in BLE DFU until an image arrives or it is reset; the bootloader does not time out (a T114 advertised for 61.5 minutes and kept going). It advertises under a name ending in `-OTA` (`HT-n5262-OTA` on a T114, not `AdaDFU`), with no service UUID in the advertisement, from the application's Bluetooth address with its lowest byte plus one. A board in its bootloader is off the mesh, so no **lnmgmt** verb brings it back. `scripts/dfu-exit.py` in the Leviculum source tree does, from a host within Bluetooth range: it connects, enables notifications on the Legacy DFU Control Point and writes its reset opcode `06`; with no transfer started the application comes back intact. A power cycle or the reset button does the same.

## OPTIONS

**-R** *transport_hash*
:   Transport identity hash of the board, 32 hexadecimal characters: the same value **lnstatus -R** takes. The management destination is derived from it.

**-i** *identity_file*
:   Path to the management identity, an RNS identity file. Mandatory: a board refuses an unidentified link.

**-w** *seconds*
:   Budget for the path and for the reply. Default 15 seconds, the reference stack's `PATH_REQUEST_TIMEOUT`, the same as **lnstatus -R**.

**--config** *dir*
:   Path to alternative Reticulum configuration directory.

**--instance-name** *name*
:   Shared-instance name to use. Default: from the configuration, else `default`.

**--no-start**
:   Fail when no shared instance answers instead of starting **lnsd**(1) as a child, as **lnstatus**(1) describes.

**-v**, **--verbose**
:   Print the steps of the request (path, link, request) on standard error.

## CONFIGURATION DIRECTORY

Without **--config**, **lnmgmt** takes the first directory of the lookup order **rnsd** uses, stated once in the installation guide, section "Configuration directory" (`docs/src/guide/installation.md:104-117`); the order is what lets Python clients find the system daemon without flags. When **lnmgmt** cannot reach or authenticate to the daemon, it names the directory it took and why, the socket it dialled, and every other candidate directory that holds a `config`, with the **--config** that selects it.

## EXIT STATUS

0 when the board accepted. 1 when it refused, for either reason. 2 on a usage error, including an identity file that cannot be read. 3 when nothing answered within **-w**: no path to the board, or no reply; the line on standard error says which. 4 on any other failure: no shared instance, the link closed, an unreadable reply.

## EXAMPLES

Put a board into BLE DFU, identified as the operator its allow-list holds:

    lnmgmt dfu -R 6a1ab9ea64747f298c1f205dfcf0f5a3 -i ~/.reticulum/mgmt_identity

Allow a minute over a slow LoRa hop:

    lnmgmt dfu -R 6a1ab9ea64747f298c1f205dfcf0f5a3 -i ~/.reticulum/mgmt_identity -w 60

## SEE ALSO

**lnstatus**(1), **lnsd**(1), **lnflash**(1)
