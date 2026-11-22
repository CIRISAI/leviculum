### State classes: hard-stranger

54728 states, 143848 transitions, 55 classes. Class: per side `phase/linkPath heard-acked path in[signals in flight] flags`.

| # | class (A \| B) | states | in (by action) | out (action -> class) | verdict |
|---|---|---|---|---|---|
| 1 | `A:Failed/relayed HA - in[-] esc+spray | B:Failed/relayed H- - in[-] esc` | 3732 | RecvOther×4422 | - | LEGAL-TERMINAL RelayedOnly |
| 2 | `A:Punching/upgrading -A - in[-] esc+spray | B:Punching/upgrading H- - in[-] esc` | 3498 | DeliverAny×108, Tick×78 | DeliverAny→#10×780, PunchFail→#23×684, PunchFail→#22×408 | LEGAL-TRANSIENT |
| 3 | `A:Connected/direct HA path in[-] esc+spray | B:Connected/direct HA path in[-] esc` | 3492 | RecvOther×4674 | - | LEGAL-TERMINAL DirectPathUp |
| 4 | `A:Punching/upgrading HA - in[-] esc+spray | B:Committing/upgrading HA path in[-] esc` | 3066 | RecvOther×3066 | PunchOK→#11×1182, PunchFail→#7×384 | LEGAL-TRANSIENT |
| 5 | `A:Punching/upgrading HA - in[COMPLETE] esc+spray | B:Committing/upgrading HA path in[-] esc` | 3066 | PunchOK×3012 | RecvOther→#4×3066, PunchOK→#19×1182, PunchFail→#15×384 | LEGAL-TRANSIENT |
| 6 | `A:Punching/upgrading HA - in[-] esc+spray | B:Punching/upgrading HA - in[-] esc` | 3012 | DeliverAny×450 | PunchOK→#5×3012, PunchOK→#21×1128, PunchFail→#14×384 | LEGAL-TRANSIENT |
| 7 | `A:Failed/relayed HA - in[-] esc+spray | B:Committing/upgrading HA path in[REJECT] esc` | 2838 | RecvOther×1572, PunchFail×384 | RecvOther→#8×2838 | LEGAL-TRANSIENT |
| 8 | `A:Failed/relayed HA - in[-] esc+spray | B:Failed/relayed HA path in[-] esc` | 2838 | RecvOther×4410 | - | LEGAL-TERMINAL RelayedOnly |
| 9 | `A:Failed/relayed -A - in[-] esc+spray | B:Failed/relayed H- - in[-] esc` | 2646 | RecvOther×2850 | - | LEGAL-TERMINAL RelayedOnly |
| 10 | `A:Punching/upgrading HA - in[-] esc+spray | B:Punching/upgrading H- - in[-] esc` | 2646 | DeliverAny×780 | PunchFail→#18×720, PunchFail→#17×522, DeliverAny→#6×450 | LEGAL-TRANSIENT |
| 11 | `A:Connected/direct HA path in[-] esc+spray | B:Committing/upgrading HA path in[COMPLETE] esc` | 2364 | PunchOK×1182, RecvOther×1182 | RecvOther→#3×2364 | LEGAL-TRANSIENT |
| 12 | `A:Committing/upgrading HA path in[COMPLETE] esc+spray | B:Connected/direct HA path in[-] esc` | 2310 | RecvOther×1182, PunchOK×1128 | RecvOther→#3×2310 | LEGAL-TRANSIENT |
| 13 | `A:Failed/relayed HA - in[-] esc+spray | B:Failed/relayed HA - in[-] esc` | 1572 | RecvOther×1572 | - | LEGAL-TERMINAL RelayedOnly |
| 14 | `A:Failed/relayed HA - in[-] esc+spray | B:Punching/upgrading HA - in[REJECT] esc` | 1572 | PunchFail×384, DeliverAny×90 | RecvOther→#13×1572, PunchOK→#15×1572 | LEGAL-TRANSIENT |
| 15 | `A:Failed/relayed HA - in[COMPLETE] esc+spray | B:Committing/upgrading HA path in[REJECT] esc` | 1572 | PunchOK×1572, PunchFail×384 | RecvOther→#7×1572, RecvOther→#16×1572 | LEGAL-TRANSIENT |
| 16 | `A:Failed/relayed HA - in[COMPLETE] esc+spray | B:Failed/relayed HA path in[-] esc` | 1572 | RecvOther×1572 | RecvOther→#8×1572 | LEGAL-TRANSIENT |
| 17 | `A:Punching/upgrading HA - in[REJECT] esc+spray | B:Failed/relayed H- - in[-] esc` | 1542 | PunchFail×522, DeliverAny×336 | RecvOther→#1×1542, PunchFail→#26×588 | LEGAL-TRANSIENT |
| 18 | `A:Failed/relayed HA - in[-] esc+spray | B:Punching/upgrading H- - in[REJECT] esc` | 1224 | PunchFail×720 | RecvOther→#1×1224, PunchFail→#26×318, DeliverAny→#14×90 | LEGAL-TRANSIENT |
| 19 | `A:Committing/upgrading HA path in[COMPLETE] esc+spray | B:Committing/upgrading HA path in[COMPLETE] esc` | 1182 | PunchOK×2310 | RecvOther→#11×1182, RecvOther→#12×1182 | LEGAL-TRANSIENT |
| 20 | `A:Committing/upgrading HA path in[-] esc+spray | B:Punching/upgrading HA - in[-] esc` | 1128 | RecvOther×1128 | PunchOK→#12×1128 | LEGAL-TRANSIENT |
| 21 | `A:Committing/upgrading HA path in[-] esc+spray | B:Punching/upgrading HA - in[COMPLETE] esc` | 1128 | PunchOK×1128 | RecvOther→#20×1128, PunchOK→#19×1128 | LEGAL-TRANSIENT |
| 22 | `A:Failed/relayed -A - in[-] esc+spray | B:Punching/upgrading H- - in[REJECT] esc` | 1062 | PunchFail×408 | RecvOther→#9×1062, PunchFail→#29×372 | LEGAL-TRANSIENT |
| 23 | `A:Punching/upgrading -A - in[REJECT] esc+spray | B:Failed/relayed H- - in[-] esc` | 924 | PunchFail×684 | RecvOther→#9×924, DeliverAny→#17×336, PunchFail→#29×120 | LEGAL-TRANSIENT |
| 24 | `A:Failed/relayed HA - in[-] esc+spray | B:Failed/relayed H- - in[REJECT] esc` | 828 | RecvOther×828 | RecvOther→#1×828 | LEGAL-TRANSIENT |
| 25 | `A:Failed/relayed HA - in[REJECT] esc+spray | B:Failed/relayed H- - in[-] esc` | 828 | RecvOther×828 | RecvOther→#1×828 | LEGAL-TRANSIENT |
| 26 | `A:Failed/relayed HA - in[REJECT] esc+spray | B:Failed/relayed H- - in[REJECT] esc` | 828 | PunchFail×906 | RecvOther→#24×828, RecvOther→#25×828 | LEGAL-TRANSIENT |
| 27 | `A:Failed/relayed -A - in[-] esc+spray | B:Failed/relayed H- - in[REJECT] esc` | 432 | RecvOther×432 | RecvOther→#9×432 | LEGAL-TRANSIENT |
| 28 | `A:Failed/relayed -A - in[REJECT] esc+spray | B:Failed/relayed H- - in[-] esc` | 432 | RecvOther×432 | RecvOther→#9×432 | LEGAL-TRANSIENT |
| 29 | `A:Failed/relayed -A - in[REJECT] esc+spray | B:Failed/relayed H- - in[REJECT] esc` | 432 | PunchFail×492 | RecvOther→#27×432, RecvOther→#28×432 | LEGAL-TRANSIENT |
| 30 | `A:Punching/upgrading -- - in[-] esc+spray | B:Punching/upgrading -- - in[-] esc` | 108 | Tick×54 | DeliverAny→#31×108 | LEGAL-TRANSIENT |
| 31 | `A:Punching/upgrading -- - in[-] esc+spray | B:Punching/upgrading H- - in[-] esc` | 108 | DeliverAny×108 | DeliverAny→#2×108 | LEGAL-TRANSIENT |
| 32 | `A:Punching/upgrading -A - in[-] esc+spray | B:Punching/upgrading H- - in[-] -` | 108 | DeliverAny×36, Tick×18 | Tick→#2×24 | LEGAL-TRANSIENT |
| 33 | `A:Punching/upgrading -A - in[-] esc | B:Punching/upgrading H- - in[-] esc` | 96 | Tick×18, DeliverAny×12 | Tick→#2×54 | LEGAL-TRANSIENT |
| 34 | `A:Punching/upgrading -- - in[-] esc | B:Punching/upgrading -- - in[-] esc` | 84 | Tick×18 | Tick→#30×54, DeliverAny→#46×12 | LEGAL-TRANSIENT |
| 35 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading -- - in[-] esc` | 78 | RecvOther×24, Tick×12 | Tick→#34×12, DeliverAny→#48×6 | LEGAL-TRANSIENT |
| 36 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading -- - in[-] -` | 63 | RecvOther×27 | Tick→#35×12, Tick→#38×9, DeliverAny→#47×6 | LEGAL-TRANSIENT |
| 37 | `A:Punching/upgrading -A - in[-] - | B:Punching/upgrading H- - in[-] esc` | 48 | Tick×6, DeliverAny×6 | Tick→#33×12 | LEGAL-TRANSIENT |
| 38 | `A:Punching/upgrading -- - in[-] esc | B:Punching/upgrading -- - in[-] -` | 36 | Tick×9 | Tick→#39×18, DeliverAny→#49×6, Tick→#34×6 | LEGAL-TRANSIENT |
| 39 | `A:Punching/upgrading -- - in[-] esc+spray | B:Punching/upgrading -- - in[-] -` | 36 | Tick×18 | DeliverAny→#40×36 | LEGAL-TRANSIENT |
| 40 | `A:Punching/upgrading -- - in[-] esc+spray | B:Punching/upgrading H- - in[-] -` | 36 | DeliverAny×36 | DeliverAny→#32×36 | LEGAL-TRANSIENT |
| 41 | `A:AwaitingReady/upgrading -- - in[READY] - | B:Punching/upgrading -- - in[-] -` | 27 | RecvOther×27, ProbeAnswer×3 | RecvOther→#36×27, Tick→#43×6 | LEGAL-TRANSIENT |
| 42 | `A:Proposing/upgrading -- - in[ACCEPT,READY] - | B:Punching/upgrading -- - in[-] -` | 27 | ProbeAnswer×3 | RecvOther→#41×27, Tick→#44×6 | LEGAL-TRANSIENT |
| 43 | `A:AwaitingReady/upgrading -- - in[READY] - | B:Punching/upgrading -- - in[-] esc` | 24 | RecvOther×24, Tick×6 | RecvOther→#35×24 | LEGAL-TRANSIENT |
| 44 | `A:Proposing/upgrading -- - in[ACCEPT,READY] - | B:Punching/upgrading -- - in[-] esc` | 24 | Tick×6 | RecvOther→#43×24 | LEGAL-TRANSIENT |
| 45 | `A:Punching/upgrading -A - in[-] esc | B:Punching/upgrading H- - in[-] -` | 18 | Tick×6, DeliverAny×6 | Tick→#32×18, Tick→#33×6 | LEGAL-TRANSIENT |
| 46 | `A:Punching/upgrading -- - in[-] esc | B:Punching/upgrading H- - in[-] esc` | 12 | DeliverAny×12 | DeliverAny→#33×12 | LEGAL-TRANSIENT |
| 47 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading H- - in[-] -` | 6 | DeliverAny×6 | DeliverAny→#50×6 | LEGAL-TRANSIENT |
| 48 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading H- - in[-] esc` | 6 | DeliverAny×6 | DeliverAny→#37×6 | LEGAL-TRANSIENT |
| 49 | `A:Punching/upgrading -- - in[-] esc | B:Punching/upgrading H- - in[-] -` | 6 | DeliverAny×6 | DeliverAny→#45×6 | LEGAL-TRANSIENT |
| 50 | `A:Punching/upgrading -A - in[-] - | B:Punching/upgrading H- - in[-] -` | 6 | DeliverAny×6 | Tick→#45×6, Tick→#37×6 | LEGAL-TRANSIENT |
| 51 | `A:AwaitingReady/upgrading -- - in[-] - | B:Discovering/upgrading -- - in[-] -` | 1 | RecvOther×1 | ProbeAnswer→#41×3 | LEGAL-TRANSIENT |
| 52 | `A:Discovering/upgrading -- - in[-] - | B:Idle/relayed -- - in[-] -` | 1 | Propose×1 | ProbeAnswer→#54×1 | LEGAL-TRANSIENT |
| 53 | `A:Idle/relayed -- - in[-] - | B:Idle/relayed -- - in[-] -` | 1 | - | Propose→#52×1 | LEGAL-TRANSIENT |
| 54 | `A:Proposing/upgrading -- - in[-] - | B:Idle/relayed -- - in[REQUEST] -` | 1 | ProbeAnswer×1 | RecvRequest→#55×1 | LEGAL-TRANSIENT |
| 55 | `A:Proposing/upgrading -- - in[ACCEPT] - | B:Discovering/upgrading -- - in[-] -` | 1 | RecvRequest×1 | ProbeAnswer→#42×3, RecvOther→#51×1 | LEGAL-TRANSIENT |

Classes with no path to a terminal state, or a rejected terminal: 0.
