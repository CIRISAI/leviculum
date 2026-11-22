### State classes: hard

100929 states, 221196 transitions, 55 classes. Class: per side `phase/linkPath heard-acked path in[signals in flight] flags`.

| # | class (A \| B) | states | in (by action) | out (action -> class) | verdict |
|---|---|---|---|---|---|
| 1 | `A:Connected/direct HA path in[-] esc+spray | B:Connected/direct HA path in[-] esc` | 8856 | RecvOther×11880 | - | LEGAL-TERMINAL DirectPathUp |
| 2 | `A:Punching/upgrading HA - in[-] esc+spray | B:Committing/upgrading HA path in[-] esc` | 6768 | RecvOther×6768 | PunchOK→#5×3024, PunchFail→#9×648 | LEGAL-TRANSIENT |
| 3 | `A:Punching/upgrading HA - in[COMPLETE] esc+spray | B:Committing/upgrading HA path in[-] esc` | 6768 | PunchOK×6552 | RecvOther→#2×6768, PunchOK→#12×3024, PunchFail→#17×648 | LEGAL-TRANSIENT |
| 4 | `A:Punching/upgrading HA - in[-] esc+spray | B:Punching/upgrading HA - in[-] esc` | 6552 | DeliverAny×792 | PunchOK→#3×6552, PunchOK→#14×2808, PunchFail→#16×648 | LEGAL-TRANSIENT |
| 5 | `A:Connected/direct HA path in[-] esc+spray | B:Committing/upgrading HA path in[COMPLETE] esc` | 6048 | PunchOK×3024, RecvOther×3024 | RecvOther→#1×6048 | LEGAL-TRANSIENT |
| 6 | `A:Committing/upgrading HA path in[COMPLETE] esc+spray | B:Connected/direct HA path in[-] esc` | 5832 | RecvOther×3024, PunchOK×2808 | RecvOther→#1×5832 | LEGAL-TRANSIENT |
| 7 | `A:Punching/upgrading -A - in[-] esc+spray | B:Punching/upgrading H- - in[-] esc` | 5400 | DeliverAny×432, Tick×288 | DeliverAny→#11×1104, PunchFail→#26×624, PunchFail→#25×384 | LEGAL-TRANSIENT |
| 8 | `A:Failed/relayed HA - in[-] esc+spray | B:Failed/relayed H- - in[-] esc` | 5280 | RecvOther×6024 | - | LEGAL-TERMINAL RelayedOnly |
| 9 | `A:Failed/relayed HA - in[-] esc+spray | B:Committing/upgrading HA path in[REJECT] esc` | 4752 | RecvOther×2592, PunchFail×648 | RecvOther→#10×4752 | LEGAL-TRANSIENT |
| 10 | `A:Failed/relayed HA - in[-] esc+spray | B:Failed/relayed HA path in[-] esc` | 4752 | RecvOther×7344 | - | LEGAL-TERMINAL RelayedOnly |
| 11 | `A:Punching/upgrading HA - in[-] esc+spray | B:Punching/upgrading H- - in[-] esc` | 4464 | DeliverAny×1104 | PunchFail→#21×912, DeliverAny→#4×792, PunchFail→#20×696 | LEGAL-TRANSIENT |
| 12 | `A:Committing/upgrading HA path in[COMPLETE] esc+spray | B:Committing/upgrading HA path in[COMPLETE] esc` | 3024 | PunchOK×5832 | RecvOther→#5×3024, RecvOther→#6×3024 | LEGAL-TRANSIENT |
| 13 | `A:Committing/upgrading HA path in[-] esc+spray | B:Punching/upgrading HA - in[-] esc` | 2808 | RecvOther×2808 | PunchOK→#6×2808 | LEGAL-TRANSIENT |
| 14 | `A:Committing/upgrading HA path in[-] esc+spray | B:Punching/upgrading HA - in[COMPLETE] esc` | 2808 | PunchOK×2808 | RecvOther→#13×2808, PunchOK→#12×2808 | LEGAL-TRANSIENT |
| 15 | `A:Failed/relayed HA - in[-] esc+spray | B:Failed/relayed HA - in[-] esc` | 2592 | RecvOther×2592 | - | LEGAL-TERMINAL RelayedOnly |
| 16 | `A:Failed/relayed HA - in[-] esc+spray | B:Punching/upgrading HA - in[REJECT] esc` | 2592 | PunchFail×648, DeliverAny×144 | RecvOther→#15×2592, PunchOK→#17×2592 | LEGAL-TRANSIENT |
| 17 | `A:Failed/relayed HA - in[COMPLETE] esc+spray | B:Committing/upgrading HA path in[REJECT] esc` | 2592 | PunchOK×2592, PunchFail×648 | RecvOther→#9×2592, RecvOther→#18×2592 | LEGAL-TRANSIENT |
| 18 | `A:Failed/relayed HA - in[COMPLETE] esc+spray | B:Failed/relayed HA path in[-] esc` | 2592 | RecvOther×2592 | RecvOther→#10×2592 | LEGAL-TRANSIENT |
| 19 | `A:Failed/relayed -A - in[-] esc+spray | B:Failed/relayed H- - in[-] esc` | 2568 | RecvOther×2616 | - | LEGAL-TERMINAL RelayedOnly |
| 20 | `A:Punching/upgrading HA - in[REJECT] esc+spray | B:Failed/relayed H- - in[-] esc` | 2304 | PunchFail×696, DeliverAny×384 | RecvOther→#8×2304, PunchFail→#24×816 | LEGAL-TRANSIENT |
| 21 | `A:Failed/relayed HA - in[-] esc+spray | B:Punching/upgrading H- - in[REJECT] esc` | 1560 | PunchFail×912 | RecvOther→#8×1560, PunchFail→#24×336, DeliverAny→#16×144 | LEGAL-TRANSIENT |
| 22 | `A:Failed/relayed HA - in[-] esc+spray | B:Failed/relayed H- - in[REJECT] esc` | 1080 | RecvOther×1080 | RecvOther→#8×1080 | LEGAL-TRANSIENT |
| 23 | `A:Failed/relayed HA - in[REJECT] esc+spray | B:Failed/relayed H- - in[-] esc` | 1080 | RecvOther×1080 | RecvOther→#8×1080 | LEGAL-TRANSIENT |
| 24 | `A:Failed/relayed HA - in[REJECT] esc+spray | B:Failed/relayed H- - in[REJECT] esc` | 1080 | PunchFail×1152 | RecvOther→#22×1080, RecvOther→#23×1080 | LEGAL-TRANSIENT |
| 25 | `A:Failed/relayed -A - in[-] esc+spray | B:Punching/upgrading H- - in[REJECT] esc` | 1032 | PunchFail×384 | RecvOther→#19×1032, PunchFail→#32×336 | LEGAL-TRANSIENT |
| 26 | `A:Punching/upgrading -A - in[REJECT] esc+spray | B:Failed/relayed H- - in[-] esc` | 816 | PunchFail×624 | RecvOther→#19×816, DeliverAny→#20×384, PunchFail→#32×96 | LEGAL-TRANSIENT |
| 27 | `A:Punching/upgrading -- - in[-] esc+spray | B:Punching/upgrading -- - in[-] esc` | 432 | Tick×216 | DeliverAny→#28×432 | LEGAL-TRANSIENT |
| 28 | `A:Punching/upgrading -- - in[-] esc+spray | B:Punching/upgrading H- - in[-] esc` | 432 | DeliverAny×432 | DeliverAny→#7×432 | LEGAL-TRANSIENT |
| 29 | `A:Punching/upgrading -A - in[-] esc+spray | B:Punching/upgrading H- - in[-] -` | 432 | DeliverAny×144, Tick×72 | Tick→#7×72 | LEGAL-TRANSIENT |
| 30 | `A:Failed/relayed -A - in[-] esc+spray | B:Failed/relayed H- - in[REJECT] esc` | 384 | RecvOther×384 | RecvOther→#19×384 | LEGAL-TRANSIENT |
| 31 | `A:Failed/relayed -A - in[REJECT] esc+spray | B:Failed/relayed H- - in[-] esc` | 384 | RecvOther×384 | RecvOther→#19×384 | LEGAL-TRANSIENT |
| 32 | `A:Failed/relayed -A - in[REJECT] esc+spray | B:Failed/relayed H- - in[REJECT] esc` | 384 | PunchFail×432 | RecvOther→#30×384, RecvOther→#31×384 | LEGAL-TRANSIENT |
| 33 | `A:Punching/upgrading -A - in[-] esc | B:Punching/upgrading H- - in[-] esc` | 384 | Tick×72, DeliverAny×48 | Tick→#7×216 | LEGAL-TRANSIENT |
| 34 | `A:Punching/upgrading -- - in[-] esc | B:Punching/upgrading -- - in[-] esc` | 336 | Tick×72 | Tick→#27×216, DeliverAny→#46×48 | LEGAL-TRANSIENT |
| 35 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading -- - in[-] esc` | 312 | RecvOther×96, Tick×48 | Tick→#34×48, DeliverAny→#48×24 | LEGAL-TRANSIENT |
| 36 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading -- - in[-] -` | 228 | RecvOther×100 | Tick→#35×48, Tick→#40×28, DeliverAny→#47×24 | LEGAL-TRANSIENT |
| 37 | `A:Punching/upgrading -A - in[-] - | B:Punching/upgrading H- - in[-] esc` | 192 | Tick×24, DeliverAny×24 | Tick→#33×48 | LEGAL-TRANSIENT |
| 38 | `A:Punching/upgrading -- - in[-] esc+spray | B:Punching/upgrading -- - in[-] -` | 144 | Tick×72 | DeliverAny→#39×144 | LEGAL-TRANSIENT |
| 39 | `A:Punching/upgrading -- - in[-] esc+spray | B:Punching/upgrading H- - in[-] -` | 144 | DeliverAny×144 | DeliverAny→#29×144 | LEGAL-TRANSIENT |
| 40 | `A:Punching/upgrading -- - in[-] esc | B:Punching/upgrading -- - in[-] -` | 128 | Tick×28 | Tick→#38×72, DeliverAny→#49×24, Tick→#34×24 | LEGAL-TRANSIENT |
| 41 | `A:AwaitingReady/upgrading -- - in[READY] - | B:Punching/upgrading -- - in[-] -` | 100 | RecvOther×100, ProbeAnswer×4 | RecvOther→#36×100, Tick→#43×24 | LEGAL-TRANSIENT |
| 42 | `A:Proposing/upgrading -- - in[ACCEPT,READY] - | B:Punching/upgrading -- - in[-] -` | 100 | ProbeAnswer×4 | RecvOther→#41×100, Tick→#44×24 | LEGAL-TRANSIENT |
| 43 | `A:AwaitingReady/upgrading -- - in[READY] - | B:Punching/upgrading -- - in[-] esc` | 96 | RecvOther×96, Tick×24 | RecvOther→#35×96 | LEGAL-TRANSIENT |
| 44 | `A:Proposing/upgrading -- - in[ACCEPT,READY] - | B:Punching/upgrading -- - in[-] esc` | 96 | Tick×24 | RecvOther→#43×96 | LEGAL-TRANSIENT |
| 45 | `A:Punching/upgrading -A - in[-] esc | B:Punching/upgrading H- - in[-] -` | 72 | Tick×24, DeliverAny×24 | Tick→#29×72, Tick→#33×24 | LEGAL-TRANSIENT |
| 46 | `A:Punching/upgrading -- - in[-] esc | B:Punching/upgrading H- - in[-] esc` | 48 | DeliverAny×48 | DeliverAny→#33×48 | LEGAL-TRANSIENT |
| 47 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading H- - in[-] -` | 24 | DeliverAny×24 | DeliverAny→#50×24 | LEGAL-TRANSIENT |
| 48 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading H- - in[-] esc` | 24 | DeliverAny×24 | DeliverAny→#37×24 | LEGAL-TRANSIENT |
| 49 | `A:Punching/upgrading -- - in[-] esc | B:Punching/upgrading H- - in[-] -` | 24 | DeliverAny×24 | DeliverAny→#45×24 | LEGAL-TRANSIENT |
| 50 | `A:Punching/upgrading -A - in[-] - | B:Punching/upgrading H- - in[-] -` | 24 | DeliverAny×24 | Tick→#45×24, Tick→#37×24 | LEGAL-TRANSIENT |
| 51 | `A:AwaitingReady/upgrading -- - in[-] - | B:Discovering/upgrading -- - in[-] -` | 1 | RecvOther×1 | ProbeAnswer→#41×4 | LEGAL-TRANSIENT |
| 52 | `A:Discovering/upgrading -- - in[-] - | B:Idle/relayed -- - in[-] -` | 1 | Propose×1 | ProbeAnswer→#54×1 | LEGAL-TRANSIENT |
| 53 | `A:Idle/relayed -- - in[-] - | B:Idle/relayed -- - in[-] -` | 1 | - | Propose→#52×1 | LEGAL-TRANSIENT |
| 54 | `A:Proposing/upgrading -- - in[-] - | B:Idle/relayed -- - in[REQUEST] -` | 1 | ProbeAnswer×1 | RecvRequest→#55×1 | LEGAL-TRANSIENT |
| 55 | `A:Proposing/upgrading -- - in[ACCEPT] - | B:Discovering/upgrading -- - in[-] -` | 1 | RecvRequest×1 | ProbeAnswer→#42×4, RecvOther→#51×1 | LEGAL-TRANSIENT |

Classes with no path to a terminal state, or a rejected terminal: 0.
