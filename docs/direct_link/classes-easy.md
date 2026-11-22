### State classes: easy

851 states, 1530 transitions, 73 classes. Class: per side `phase/linkPath heard-acked path in[signals in flight] flags`.

| # | class (A \| B) | states | in (by action) | out (action -> class) | verdict |
|---|---|---|---|---|---|
| 1 | `A:Connected/direct HA path in[-] esc | B:Connected/direct HA path in[-] esc` | 123 | RecvOther×164 | - | LEGAL-TERMINAL DirectPathUp |
| 2 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Connected/direct HA path in[-] esc` | 82 | PunchOK×41, RecvOther×41 | RecvOther→#1×82 | LEGAL-TRANSIENT |
| 3 | `A:Connected/direct HA path in[-] esc | B:Committing/upgrading HA path in[COMPLETE] esc` | 82 | RecvOther×41, PunchOK×41 | RecvOther→#1×82 | LEGAL-TRANSIENT |
| 4 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading HA - in[-] esc` | 41 | RecvOther×41, DeliverAny×5, Tick×2 | PunchOK→#2×41 | LEGAL-TRANSIENT |
| 5 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading HA - in[COMPLETE] esc` | 41 | PunchOK×41, DeliverAny×5, Tick×2 | RecvOther→#4×41, PunchOK→#6×41 | LEGAL-TRANSIENT |
| 6 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Committing/upgrading HA path in[COMPLETE] esc` | 41 | PunchOK×82 | RecvOther→#3×41, RecvOther→#2×41 | LEGAL-TRANSIENT |
| 7 | `A:Punching/upgrading HA - in[-] esc | B:Committing/upgrading HA path in[-] esc` | 41 | RecvOther×41, DeliverAny×5, Tick×2 | PunchOK→#3×41 | LEGAL-TRANSIENT |
| 8 | `A:Punching/upgrading HA - in[-] esc | B:Punching/upgrading HA - in[-] esc` | 41 | DeliverAny×10, Tick×4 | PunchOK→#5×41, PunchOK→#9×41 | LEGAL-TRANSIENT |
| 9 | `A:Punching/upgrading HA - in[COMPLETE] esc | B:Committing/upgrading HA path in[-] esc` | 41 | PunchOK×41, DeliverAny×5, Tick×2 | RecvOther→#7×41, PunchOK→#6×41 | LEGAL-TRANSIENT |
| 10 | `A:Connected/direct HA path in[-] - | B:Connected/direct HA path in[-] esc` | 18 | RecvOther×25 | - | LEGAL-TERMINAL DirectPathUp |
| 11 | `A:Connected/direct HA path in[-] esc | B:Connected/direct HA path in[-] -` | 18 | RecvOther×25 | - | LEGAL-TERMINAL DirectPathUp |
| 12 | `A:Committing/upgrading HA path in[COMPLETE] - | B:Connected/direct HA path in[-] esc` | 14 | PunchOK×7, RecvOther×7 | RecvOther→#10×14 | LEGAL-TRANSIENT |
| 13 | `A:Connected/direct HA path in[-] esc | B:Committing/upgrading HA path in[COMPLETE] -` | 14 | PunchOK×7, RecvOther×7 | RecvOther→#11×14 | LEGAL-TRANSIENT |
| 14 | `A:Punching/upgrading -A - in[-] esc | B:Punching/upgrading H- - in[-] esc` | 14 | Tick×3 | DeliverAny→#36×5, PunchFail→#24×2 | LEGAL-TRANSIENT |
| 15 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading -A - in[-] esc` | 14 | Tick×3 | DeliverAny→#34×5, PunchFail→#26×2 | LEGAL-TRANSIENT |
| 16 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Connected/direct HA path in[-] -` | 11 | RecvOther×7, PunchOK×4 | RecvOther→#11×11 | LEGAL-TRANSIENT |
| 17 | `A:Connected/direct HA path in[-] - | B:Committing/upgrading HA path in[COMPLETE] esc` | 11 | RecvOther×7, PunchOK×4 | RecvOther→#10×11 | LEGAL-TRANSIENT |
| 18 | `A:Failed/relayed -A - in[-] esc | B:Failed/relayed H- - in[-] esc` | 9 | RecvOther×11 | - | LEGAL-TERMINAL RelayedOnly |
| 19 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed -A - in[-] esc` | 9 | RecvOther×11 | - | LEGAL-TERMINAL RelayedOnly |
| 20 | `A:Committing/upgrading HA path in[-] - | B:Punching/upgrading HA - in[-] esc` | 7 | RecvOther×7, DeliverAny×2 | PunchOK→#12×7 | LEGAL-TRANSIENT |
| 21 | `A:Committing/upgrading HA path in[-] - | B:Punching/upgrading HA - in[COMPLETE] esc` | 7 | PunchOK×4, DeliverAny×2 | RecvOther→#20×7, PunchOK→#22×7 | LEGAL-TRANSIENT |
| 22 | `A:Committing/upgrading HA path in[COMPLETE] - | B:Committing/upgrading HA path in[COMPLETE] esc` | 7 | PunchOK×11 | RecvOther→#17×7, RecvOther→#12×7 | LEGAL-TRANSIENT |
| 23 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Committing/upgrading HA path in[COMPLETE] -` | 7 | PunchOK×11 | RecvOther→#13×7, RecvOther→#16×7 | LEGAL-TRANSIENT |
| 24 | `A:Failed/relayed -A - in[-] esc | B:Punching/upgrading H- - in[REJECT] esc` | 7 | PunchFail×2 | RecvOther→#18×7, PunchFail→#54×2 | LEGAL-TRANSIENT |
| 25 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading -- - in[-] -` | 7 | RecvOther×3 | Tick→#45×1, DeliverAny→#68×1, Tick→#46×1, DeliverAny→#71×1 | LEGAL-TRANSIENT |
| 26 | `A:Punching/upgrading H- - in[REJECT] esc | B:Failed/relayed -A - in[-] esc` | 7 | PunchFail×2 | RecvOther→#19×7, PunchFail→#57×2 | LEGAL-TRANSIENT |
| 27 | `A:Punching/upgrading HA - in[-] esc | B:Committing/upgrading HA path in[-] -` | 7 | RecvOther×7, DeliverAny×2 | PunchOK→#13×7 | LEGAL-TRANSIENT |
| 28 | `A:Punching/upgrading HA - in[COMPLETE] esc | B:Committing/upgrading HA path in[-] -` | 7 | PunchOK×4, DeliverAny×2 | RecvOther→#27×7, PunchOK→#23×7 | LEGAL-TRANSIENT |
| 29 | `A:Punching/upgrading -A - in[-] esc | B:Punching/upgrading H- - in[-] -` | 6 | Tick×1 | Tick→#14×2 | LEGAL-TRANSIENT |
| 30 | `A:Punching/upgrading H- - in[-] - | B:Punching/upgrading -A - in[-] esc` | 6 | Tick×1 | Tick→#15×2 | LEGAL-TRANSIENT |
| 31 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading H- - in[-] esc` | 5 | RecvOther×5 | DeliverAny→#4×5 | LEGAL-TRANSIENT |
| 32 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading H- - in[COMPLETE] esc` | 5 | PunchOK×5 | RecvOther→#31×5, DeliverAny→#5×5 | LEGAL-TRANSIENT |
| 33 | `A:Punching/upgrading H- - in[-] esc | B:Committing/upgrading HA path in[-] esc` | 5 | RecvOther×5 | DeliverAny→#7×5 | LEGAL-TRANSIENT |
| 34 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading HA - in[-] esc` | 5 | DeliverAny×5 | PunchOK→#35×5, DeliverAny→#8×5 | LEGAL-TRANSIENT |
| 35 | `A:Punching/upgrading H- - in[COMPLETE] esc | B:Committing/upgrading HA path in[-] esc` | 5 | PunchOK×5 | RecvOther→#33×5, DeliverAny→#9×5 | LEGAL-TRANSIENT |
| 36 | `A:Punching/upgrading HA - in[-] esc | B:Punching/upgrading H- - in[-] esc` | 5 | DeliverAny×5 | PunchOK→#32×5, DeliverAny→#8×5 | LEGAL-TRANSIENT |
| 37 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading HA - in[-] -` | 4 | RecvOther×4 | PunchOK→#16×4, Tick→#4×2 | LEGAL-TRANSIENT |
| 38 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading HA - in[COMPLETE] -` | 4 | PunchOK×4 | RecvOther→#37×4, PunchOK→#23×4, Tick→#5×2 | LEGAL-TRANSIENT |
| 39 | `A:Punching/upgrading HA - in[-] - | B:Committing/upgrading HA path in[-] esc` | 4 | RecvOther×4 | PunchOK→#17×4, Tick→#7×2 | LEGAL-TRANSIENT |
| 40 | `A:Punching/upgrading HA - in[-] - | B:Punching/upgrading HA - in[-] esc` | 4 | DeliverAny×2 | PunchOK→#21×4, PunchOK→#42×4, Tick→#8×2 | LEGAL-TRANSIENT |
| 41 | `A:Punching/upgrading HA - in[-] esc | B:Punching/upgrading HA - in[-] -` | 4 | DeliverAny×2 | PunchOK→#38×4, PunchOK→#28×4, Tick→#8×2 | LEGAL-TRANSIENT |
| 42 | `A:Punching/upgrading HA - in[COMPLETE] - | B:Committing/upgrading HA path in[-] esc` | 4 | PunchOK×4 | RecvOther→#39×4, PunchOK→#22×4, Tick→#9×2 | LEGAL-TRANSIENT |
| 43 | `A:AwaitingReady/upgrading -- - in[READY] - | B:Punching/upgrading -- - in[-] -` | 3 | RecvOther×3, ProbeAnswer×1 | RecvOther→#25×3, Tick→#49×1 | LEGAL-TRANSIENT |
| 44 | `A:Proposing/upgrading -- - in[ACCEPT,READY] - | B:Punching/upgrading -- - in[-] -` | 3 | ProbeAnswer×1 | RecvOther→#43×3, Tick→#58×1 | LEGAL-TRANSIENT |
| 45 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading -- - in[-] esc` | 3 | RecvOther×2, Tick×1 | DeliverAny→#69×1 | LEGAL-TRANSIENT |
| 46 | `A:Punching/upgrading -- - in[-] esc | B:Punching/upgrading -- - in[-] -` | 3 | Tick×1 | DeliverAny→#73×1 | LEGAL-TRANSIENT |
| 47 | `A:Punching/upgrading -A - in[-] - | B:Punching/upgrading H- - in[-] esc` | 3 | Tick×1, DeliverAny×1 | DeliverAny→#62×2, Tick→#14×1 | LEGAL-TRANSIENT |
| 48 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading -A - in[-] -` | 3 | Tick×1, DeliverAny×1 | DeliverAny→#60×2, Tick→#15×1 | LEGAL-TRANSIENT |
| 49 | `A:AwaitingReady/upgrading -- - in[READY] - | B:Punching/upgrading -- - in[-] esc` | 2 | RecvOther×2, Tick×1 | RecvOther→#45×2 | LEGAL-TRANSIENT |
| 50 | `A:Committing/upgrading HA path in[-] - | B:Punching/upgrading H- - in[-] esc` | 2 | RecvOther×2 | DeliverAny→#20×2 | LEGAL-TRANSIENT |
| 51 | `A:Committing/upgrading HA path in[-] - | B:Punching/upgrading H- - in[COMPLETE] esc` | 2 | PunchOK×2 | RecvOther→#50×2, DeliverAny→#21×2 | LEGAL-TRANSIENT |
| 52 | `A:Failed/relayed -A - in[-] esc | B:Failed/relayed H- - in[REJECT] esc` | 2 | RecvOther×2 | RecvOther→#18×2 | LEGAL-TRANSIENT |
| 53 | `A:Failed/relayed -A - in[REJECT] esc | B:Failed/relayed H- - in[-] esc` | 2 | RecvOther×2 | RecvOther→#18×2 | LEGAL-TRANSIENT |
| 54 | `A:Failed/relayed -A - in[REJECT] esc | B:Failed/relayed H- - in[REJECT] esc` | 2 | PunchFail×2 | RecvOther→#52×2, RecvOther→#53×2 | LEGAL-TRANSIENT |
| 55 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed -A - in[REJECT] esc` | 2 | RecvOther×2 | RecvOther→#19×2 | LEGAL-TRANSIENT |
| 56 | `A:Failed/relayed H- - in[REJECT] esc | B:Failed/relayed -A - in[-] esc` | 2 | RecvOther×2 | RecvOther→#19×2 | LEGAL-TRANSIENT |
| 57 | `A:Failed/relayed H- - in[REJECT] esc | B:Failed/relayed -A - in[REJECT] esc` | 2 | PunchFail×2 | RecvOther→#55×2, RecvOther→#56×2 | LEGAL-TRANSIENT |
| 58 | `A:Proposing/upgrading -- - in[ACCEPT,READY] - | B:Punching/upgrading -- - in[-] esc` | 2 | Tick×1 | RecvOther→#49×2 | LEGAL-TRANSIENT |
| 59 | `A:Punching/upgrading H- - in[-] esc | B:Committing/upgrading HA path in[-] -` | 2 | RecvOther×2 | DeliverAny→#27×2 | LEGAL-TRANSIENT |
| 60 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading HA - in[-] -` | 2 | DeliverAny×2 | PunchOK→#61×2, DeliverAny→#41×2 | LEGAL-TRANSIENT |
| 61 | `A:Punching/upgrading H- - in[COMPLETE] esc | B:Committing/upgrading HA path in[-] -` | 2 | PunchOK×2 | RecvOther→#59×2, DeliverAny→#28×2 | LEGAL-TRANSIENT |
| 62 | `A:Punching/upgrading HA - in[-] - | B:Punching/upgrading H- - in[-] esc` | 2 | DeliverAny×2 | PunchOK→#51×2, DeliverAny→#40×2 | LEGAL-TRANSIENT |
| 63 | `A:AwaitingReady/upgrading -- - in[-] - | B:Discovering/upgrading -- - in[-] -` | 1 | RecvOther×1 | ProbeAnswer→#43×1 | LEGAL-TRANSIENT |
| 64 | `A:Discovering/upgrading -- - in[-] - | B:Idle/relayed -- - in[-] -` | 1 | Propose×1 | ProbeAnswer→#66×1 | LEGAL-TRANSIENT |
| 65 | `A:Idle/relayed -- - in[-] - | B:Idle/relayed -- - in[-] -` | 1 | - | Propose→#64×1 | LEGAL-TRANSIENT |
| 66 | `A:Proposing/upgrading -- - in[-] - | B:Idle/relayed -- - in[REQUEST] -` | 1 | ProbeAnswer×1 | RecvRequest→#67×1 | LEGAL-TRANSIENT |
| 67 | `A:Proposing/upgrading -- - in[ACCEPT] - | B:Discovering/upgrading -- - in[-] -` | 1 | RecvRequest×1 | RecvOther→#63×1, ProbeAnswer→#44×1 | LEGAL-TRANSIENT |
| 68 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading H- - in[-] -` | 1 | DeliverAny×1 | DeliverAny→#70×1 | LEGAL-TRANSIENT |
| 69 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading H- - in[-] esc` | 1 | DeliverAny×1 | DeliverAny→#47×1 | LEGAL-TRANSIENT |
| 70 | `A:Punching/upgrading -A - in[-] - | B:Punching/upgrading H- - in[-] -` | 1 | DeliverAny×1 | Tick→#29×1, Tick→#47×1 | LEGAL-TRANSIENT |
| 71 | `A:Punching/upgrading H- - in[-] - | B:Punching/upgrading -- - in[-] -` | 1 | DeliverAny×1 | DeliverAny→#72×1 | LEGAL-TRANSIENT |
| 72 | `A:Punching/upgrading H- - in[-] - | B:Punching/upgrading -A - in[-] -` | 1 | DeliverAny×1 | Tick→#48×1, Tick→#30×1 | LEGAL-TRANSIENT |
| 73 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading -- - in[-] -` | 1 | DeliverAny×1 | DeliverAny→#48×1 | LEGAL-TRANSIENT |

Classes with no path to a terminal state, or a rejected terminal: 0.
