# Security policy

Leviculum is a mesh networking stack whose headline property is that
traffic is end to end encrypted. A flaw in it is not a flaw in one
program: it is a flaw in the privacy of everyone whose packets cross a
node running this code. So there is a private way to tell us about one,
and it is described here rather than assumed.

## Reporting a vulnerability

Send it by e-mail to lp@lew-palm.de. One person reads that address and it
is the same address every commit in this repository is authored under, so
it is not a role account that may or may not be staffed.

Please do not open a tracker issue for a suspected vulnerability. Codeberg
issues are public from the moment they are filed, which publishes the flaw
to everyone who might use it before anyone can ship a fix.

A first message needs nothing more than enough to tell whether the report
is real: which component, what an attacker gains, and how to reproduce it.
A proof of concept helps and is not required. If you would rather encrypt,
say so in a message with no detail in it and we will arrange a key; the
project publishes no OpenPGP key today and claiming one it does not use
would be worse than saying this.

Reports about the Reticulum protocol itself, as opposed to this
implementation of it, are welcome here too. Say which one you think it is
and we will work it out together.

There is no LXMF address for this, which for this project would otherwise
be the fitting channel. An address is only a channel if somebody reads it,
and an unread one is worse than none because it still looks like a way in.
When one is monitored it will be named here.

## What to expect

You get an acknowledgement within 7 days that a human has read the report
and whether we can reproduce it. If that window passes in silence, assume
the mail was lost rather than ignored and send it again.

Coordinated disclosure is offered and preferred. The default is that the
details stay between us for 90 days from the acknowledgement, or until a
fix is released, whichever comes first, and then either side may publish.
We will ask for more time only with a reason and a date, and a refusal is
yours to give. You decide whether to be credited by name in the changelog
entry and in the commit that carries the fix.

What we will not do is ask you to stay quiet indefinitely, or treat a
report made in good faith as an attack on the project.

## Supported versions

One line is supported: the current one. Fixes land on `master` and reach
users through the next nightly package, and through the next tagged
release. Older tags get nothing backported to them, because the project
has one rolling line of development and pretending otherwise would promise
a branch nobody maintains.

If you run a nightly, `lnsd --version` prints the exact build it came
from; include that string in a report.

## Scope

In scope is anything in this repository: the protocol core, the daemon and
its shared-instance IPC, the interfaces, the firmware, and the packaging.
Cryptographic mistakes, anything that lets a node read or forge traffic it
should not, anything reachable from the network that crashes or hangs a
node, and privilege problems in the Debian packages all count.

Out of scope are findings that need an attacker who already has root on
the machine, missing hardening that has no attack behind it, and reports
produced by a scanner with nothing shown to follow from them. A radio
denial of service by transmitting on the same frequency is a property of
shared spectrum rather than a defect in this code.
