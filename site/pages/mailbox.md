+++
title = "Your mailbox"
+++

Your mailbox is a computer at home that stays on, a Raspberry Pi for instance. It receives your messages while your phone is off and keeps them until the phone collects them. You set it up once, with one command, and then scan a code with your phone.

## 1. Install Loquax on the computer

Download two files from the [download page](/download): the stack (`leviculum`) and the messenger (`loquax`), `amd64` for a PC, `arm64` for a Raspberry Pi with the 64-bit system. Then, in the folder holding both:

```
sudo apt install ./leviculum-nightly-amd64.deb ./loquax-nightly-amd64.deb
```

On a Raspberry Pi the names end in `arm64.deb` instead.

## 2. Run the one command

```
loquax mailbox setup
```

It asks two questions, where to keep your mail and how your phone reaches the computer. Press Enter for the suggested answer to each. To start the mailbox with the computer it needs administrator rights once, so it may ask for your password. At the end it shows a code, as text and as a square QR code.

## 3. Scan the code

On your phone, open Loquax (the app is on the [download page](/download) too), choose "On a computer at home", then "Scan the code", and point the camera at the square. The phone connects to your mailbox by itself. Each code works for one phone, once; to add another device, run `loquax mailbox setup` again for a new code.
