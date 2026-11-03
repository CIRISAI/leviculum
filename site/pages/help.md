+++
title = "Help"
+++

How to use Loquax, the messenger app. [Diese Seite auf Deutsch](/help-de).

## First steps

When you open Loquax for the first time, it asks where your mailbox should be. Your mailbox receives your messages and keeps them for you.

- "On this phone" is ready at once. Messages reach you while the phone is on and online.
- "On a computer at home" means messages arrive even when your phone is off. You need a computer that stays on, see "The mailbox" below.

You can start on the phone and move your mailbox to a computer later without losing anything.

Then Loquax shows your address, which is how people write to you, and asks for a name others will see. Save a backup of your address when it offers one: without it, a lost phone means a new address.

Allow Loquax to run in the background when Android asks. Otherwise Android stops it when the phone lies still, and your messages arrive late.

## Adding a contact

Every Loquax user has a contact code, shown as a square QR code on their profile. Tap your own picture at the top of the chat list to see yours.

- **Together in one place:** on your profile, tap "Scan a contact's code" and point the camera at the other phone's square. Loquax asks what you call this contact, then opens the chat.
- **Far apart:** ask the other person to tap "Share" on their profile and send you the text it makes, by email or any other messenger. It starts with `lxma://`. Copy it, tap the pencil in the chat list, paste it into the field and confirm the name.
- **To invite someone:** tap "Share" on your profile and send them the text, or "Share as picture" to send the square itself.

Codes from Columba and Sideband work the same way.

## Sending photos, files and voice messages

- **A photo:** tap the camera in the message field to take one, or the plus next to it, then "Gallery". You can add a caption before you send it. Loquax makes photos smaller before they go, and their place and camera data never go along. How small is under Settings, Chats, "Photo size".
- **A file:** the plus, then "File".
- **A voice message:** hold the microphone while you speak and let go to send. Slide left to cancel, or slide up to lock the recording so you can let go of the button.

## Sending your position

In a chat, tap the position mark in the message field. Loquax asks once for permission to use your location, then offers "Now, once", or for 15 minutes, 1 hour, 8 hours or "Until I stop". Only this one person sees it, and Loquax sends your position only when you choose so.

While you share, the chat shows how long is left and a "Stop" button. Settings, Privacy, "Position sharing" lists everyone you share with right now.

You can also share from the map: "Share my position" asks whom it should go to. When a friend shares theirs, Loquax marks them "live" and shows them on the map.

## Groups

Tap the pencil in the chat list, then "New group". Pick the people, tap "Next", give the group a name and tap "Create". Everyone you picked gets an invitation with a "Join" button.

A group lives on the mailbox of the person who made it, and that mailbox can read the group's messages. Loquax says so on the invitation, so you know whom you trust.

In the group's chat, the menu's "Members" shows who is in it. There you can leave the group; the person who made it can also add and remove members and delete the group for everyone.

Friends who use another app, Columba or Sideband for instance, can be in a group too: they get the invitation and the group's messages as ordinary messages.

## The mailbox

Your mailbox is a computer at home that stays on, a Raspberry Pi for instance, and keeps your messages while your phone is off.

Install Loquax on that computer (the [mailbox page](/mailbox) says how) and run the one command:

```
loquax mailbox setup
```

Press Enter for the suggested answers. At the end it shows a square code. On your phone choose "On a computer at home", then "Scan the code", and point the camera at the square. The phone connects to your mailbox by itself.

Already using Loquax with the mailbox on the phone? Open your profile and tap "Use another mailbox".

## A second device

A tablet or a second phone can read the same mailbox.

- **Mailbox on your phone:** tap your picture, then "Link a device". On the other device, install Loquax, choose "Join an existing mailbox" and scan the code.
- **Mailbox on a computer:** run `loquax mailbox setup` on the computer again: each code works for one device, once. Then install Loquax on the second device and scan the new code as above.

A new phone that replaces the old one is a move, not a second device: on the old phone tap your picture, then "Move to a new phone". Your address, contacts and messages move over.

Both devices then see the same chats. When you delete a message, Loquax asks whether to delete it on this device only or on every device of the mailbox.

## What the ticks mean

Your own messages carry a small mark in the corner:

- **A clock:** Loquax is still sending, or has sent and waits for an answer.
- **One tick:** the person you wrote to is not reachable right now. The message waits in the network for them and reaches them once their phone or mailbox collects it.
- **Two ticks:** delivered.
- **Two green ticks:** read. You only see this when the other person allows read receipts, and only while you allow them too (Settings, Privacy).
- **A red circle:** not sent. Tap it to send again.

## When "Not connected" stays

Loquax tells you in its notification whether it reaches your mailbox. If it keeps saying "Not connected":

1. Check that the phone has internet, Wi-Fi or mobile data.
2. Mailbox on a computer: check that the computer is on and online. When the setup was allowed to start the mailbox with the computer, it comes back by itself after a power cut.
3. Check that Loquax may run in the background: in Android's settings for Loquax, battery, choose unrestricted.
4. Still nothing? On the computer, run `sudo systemctl restart loquax-mailbox` and wait a minute.
5. If you moved the computer to another network and chose a fixed address during setup, run `loquax mailbox setup` again and scan the new code.

Messages you write meanwhile are kept and go out once the connection is back.

## How it works, if you want to know

Loquax does not use a company's servers. Each message is encrypted for the one person it is for and travels through a mesh of computers, phones and small radio boards that pass it on, over the internet, Wi-Fi, Bluetooth, or LoRa radio where there is no internet at all. Your address is derived from a key that only your device holds. The details are in the [developer documentation](https://leviculum.network/docs/).
