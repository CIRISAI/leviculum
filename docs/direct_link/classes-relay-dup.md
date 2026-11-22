### State classes: relay-dup

638 states, 1100 transitions, 107 classes. Class: per side `phase/linkPath heard-acked path in[signals in flight] flags`.

| # | class (A \| B) | states | in (by action) | out (action -> class) | verdict |
|---|---|---|---|---|---|
| 1 | `A:Connected/direct HA path in[-] esc | B:Connected/direct HA path in[-] esc` | 60 | RecvOther×80 | - | LEGAL-TERMINAL DirectPathUp |
| 2 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Connected/direct HA path in[-] esc` | 40 | PunchOK×20, RecvOther×20 | RecvOther→#1×40 | LEGAL-TRANSIENT |
| 3 | `A:Connected/direct HA path in[-] esc | B:Committing/upgrading HA path in[COMPLETE] esc` | 40 | RecvOther×20, PunchOK×20 | RecvOther→#1×40 | LEGAL-TRANSIENT |
| 4 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading HA - in[-] esc` | 20 | RecvOther×20, DeliverAny×4, Tick×2 | PunchOK→#2×20 | LEGAL-TRANSIENT |
| 5 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading HA - in[COMPLETE] esc` | 20 | PunchOK×20, DeliverAny×4, Tick×2 | RecvOther→#4×20, PunchOK→#6×20 | LEGAL-TRANSIENT |
| 6 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Committing/upgrading HA path in[COMPLETE] esc` | 20 | PunchOK×40 | RecvOther→#3×20, RecvOther→#2×20 | LEGAL-TRANSIENT |
| 7 | `A:Punching/upgrading HA - in[-] esc | B:Committing/upgrading HA path in[-] esc` | 20 | RecvOther×20, DeliverAny×4, Tick×2 | PunchOK→#3×20 | LEGAL-TRANSIENT |
| 8 | `A:Punching/upgrading HA - in[-] esc | B:Punching/upgrading HA - in[-] esc` | 20 | DeliverAny×8, Tick×4 | PunchOK→#5×20, PunchOK→#9×20 | LEGAL-TRANSIENT |
| 9 | `A:Punching/upgrading HA - in[COMPLETE] esc | B:Committing/upgrading HA path in[-] esc` | 20 | PunchOK×20, DeliverAny×4, Tick×2 | RecvOther→#7×20, PunchOK→#6×20 | LEGAL-TRANSIENT |
| 10 | `A:Connected/direct HA path in[-] - | B:Connected/direct HA path in[-] esc` | 12 | RecvOther×16 | - | LEGAL-TERMINAL DirectPathUp |
| 11 | `A:Connected/direct HA path in[-] esc | B:Connected/direct HA path in[-] -` | 12 | RecvOther×16 | - | LEGAL-TERMINAL DirectPathUp |
| 12 | `A:Failed/relayed -A - in[-] esc | B:Failed/relayed H- - in[-] esc` | 12 | RecvOther×12 | - | LEGAL-TERMINAL RelayedOnly |
| 13 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed -A - in[-] esc` | 12 | RecvOther×12 | - | LEGAL-TERMINAL RelayedOnly |
| 14 | `A:Punching/upgrading -A - in[-] esc | B:Punching/upgrading H- - in[-] esc` | 10 | Tick×3 | DeliverAny→#51×4, PunchFail→#23×2, PunchFail→#89×2 | LEGAL-TRANSIENT |
| 15 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading -A - in[-] esc` | 10 | Tick×3 | DeliverAny→#46×4, PunchFail→#79×2, PunchFail→#35×2 | LEGAL-TRANSIENT |
| 16 | `A:Committing/upgrading HA path in[COMPLETE] - | B:Connected/direct HA path in[-] esc` | 8 | PunchOK×4, RecvOther×4 | RecvOther→#10×8 | LEGAL-TRANSIENT |
| 17 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Connected/direct HA path in[-] -` | 8 | PunchOK×4, RecvOther×4 | RecvOther→#11×8 | LEGAL-TRANSIENT |
| 18 | `A:Connected/direct HA path in[-] - | B:Committing/upgrading HA path in[COMPLETE] esc` | 8 | RecvOther×4, PunchOK×4 | RecvOther→#10×8 | LEGAL-TRANSIENT |
| 19 | `A:Connected/direct HA path in[-] esc | B:Committing/upgrading HA path in[COMPLETE] -` | 8 | RecvOther×4, PunchOK×4 | RecvOther→#11×8 | LEGAL-TRANSIENT |
| 20 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading -- - in[-] -` | 7 | RecvOther×3 | Tick→#61×1, Tick→#60×1, DeliverAny→#105×1, DeliverAny→#101×1 | LEGAL-TRANSIENT |
| 21 | `A:Committing/upgrading HA path in[REJECT] esc | B:Failed/relayed H- - in[-] esc` | 6 | RecvOther×6 | RecvOther→#31×6 | LEGAL-TRANSIENT |
| 22 | `A:Committing/upgrading HA path in[REJECT] esc | B:Failed/relayed H- - in[COMPLETE] esc` | 6 | PunchOK×6 | RecvOther→#32×6, RecvOther→#21×6 | LEGAL-TRANSIENT |
| 23 | `A:Failed/relayed -A - in[-] esc | B:Punching/upgrading H- - in[REJECT] esc` | 6 | PunchFail×2, Tick×1 | RecvOther→#12×6, PunchFail→#73×2 | LEGAL-TRANSIENT |
| 24 | `A:Failed/relayed H- - in[-] esc | B:Committing/upgrading HA path in[REJECT] esc` | 6 | RecvOther×6 | RecvOther→#26×6 | LEGAL-TRANSIENT |
| 25 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed HA - in[-] esc` | 6 | RecvOther×6 | - | LEGAL-TERMINAL RelayedOnly |
| 26 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed HA path in[-] esc` | 6 | RecvOther×12 | - | LEGAL-TERMINAL RelayedOnly |
| 27 | `A:Failed/relayed H- - in[-] esc | B:Punching/upgrading HA - in[REJECT] esc` | 6 | DeliverAny×2, Tick×1 | RecvOther→#25×6, PunchOK→#28×6 | LEGAL-TRANSIENT |
| 28 | `A:Failed/relayed H- - in[COMPLETE] esc | B:Committing/upgrading HA path in[REJECT] esc` | 6 | PunchOK×6 | RecvOther→#24×6, RecvOther→#29×6 | LEGAL-TRANSIENT |
| 29 | `A:Failed/relayed H- - in[COMPLETE] esc | B:Failed/relayed HA path in[-] esc` | 6 | RecvOther×6 | RecvOther→#26×6 | LEGAL-TRANSIENT |
| 30 | `A:Failed/relayed HA - in[-] esc | B:Failed/relayed H- - in[-] esc` | 6 | RecvOther×6 | - | LEGAL-TERMINAL RelayedOnly |
| 31 | `A:Failed/relayed HA path in[-] esc | B:Failed/relayed H- - in[-] esc` | 6 | RecvOther×12 | - | LEGAL-TERMINAL RelayedOnly |
| 32 | `A:Failed/relayed HA path in[-] esc | B:Failed/relayed H- - in[COMPLETE] esc` | 6 | RecvOther×6 | RecvOther→#31×6 | LEGAL-TRANSIENT |
| 33 | `A:Punching/upgrading -A - in[-] esc | B:Punching/upgrading H- - in[-] -` | 6 | Tick×1 | Tick→#14×2, PunchFail→#57×2 | LEGAL-TRANSIENT |
| 34 | `A:Punching/upgrading H- - in[-] - | B:Punching/upgrading -A - in[-] esc` | 6 | Tick×1 | Tick→#15×2, PunchFail→#64×2 | LEGAL-TRANSIENT |
| 35 | `A:Punching/upgrading H- - in[REJECT] esc | B:Failed/relayed -A - in[-] esc` | 6 | PunchFail×2, Tick×1 | RecvOther→#13×6, PunchFail→#84×2 | LEGAL-TRANSIENT |
| 36 | `A:Punching/upgrading HA - in[REJECT] esc | B:Failed/relayed H- - in[-] esc` | 6 | DeliverAny×2, Tick×1 | RecvOther→#30×6, PunchOK→#22×6 | LEGAL-TRANSIENT |
| 37 | `A:Committing/upgrading HA path in[-] - | B:Punching/upgrading HA - in[-] esc` | 4 | RecvOther×4, DeliverAny×2 | PunchOK→#16×4 | LEGAL-TRANSIENT |
| 38 | `A:Committing/upgrading HA path in[-] - | B:Punching/upgrading HA - in[COMPLETE] esc` | 4 | PunchOK×4, DeliverAny×2 | RecvOther→#37×4, PunchOK→#43×4 | LEGAL-TRANSIENT |
| 39 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading H- - in[-] esc` | 4 | RecvOther×4 | DeliverAny→#4×4 | LEGAL-TRANSIENT |
| 40 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading H- - in[COMPLETE] esc` | 4 | PunchOK×4 | RecvOther→#39×4, DeliverAny→#5×4 | LEGAL-TRANSIENT |
| 41 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading HA - in[-] -` | 4 | RecvOther×4 | PunchOK→#17×4, Tick→#4×2 | LEGAL-TRANSIENT |
| 42 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading HA - in[COMPLETE] -` | 4 | PunchOK×4 | RecvOther→#41×4, PunchOK→#44×4, Tick→#5×2 | LEGAL-TRANSIENT |
| 43 | `A:Committing/upgrading HA path in[COMPLETE] - | B:Committing/upgrading HA path in[COMPLETE] esc` | 4 | PunchOK×8 | RecvOther→#18×4, RecvOther→#16×4 | LEGAL-TRANSIENT |
| 44 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Committing/upgrading HA path in[COMPLETE] -` | 4 | PunchOK×8 | RecvOther→#19×4, RecvOther→#17×4 | LEGAL-TRANSIENT |
| 45 | `A:Punching/upgrading H- - in[-] esc | B:Committing/upgrading HA path in[-] esc` | 4 | RecvOther×4 | DeliverAny→#7×4 | LEGAL-TRANSIENT |
| 46 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading HA - in[-] esc` | 4 | DeliverAny×4 | PunchOK→#47×4, DeliverAny→#8×4 | LEGAL-TRANSIENT |
| 47 | `A:Punching/upgrading H- - in[COMPLETE] esc | B:Committing/upgrading HA path in[-] esc` | 4 | PunchOK×4 | RecvOther→#45×4, DeliverAny→#9×4 | LEGAL-TRANSIENT |
| 48 | `A:Punching/upgrading HA - in[-] - | B:Committing/upgrading HA path in[-] esc` | 4 | RecvOther×4 | PunchOK→#18×4, Tick→#7×2 | LEGAL-TRANSIENT |
| 49 | `A:Punching/upgrading HA - in[-] - | B:Punching/upgrading HA - in[-] esc` | 4 | DeliverAny×2 | PunchOK→#38×4, PunchOK→#53×4, Tick→#8×2 | LEGAL-TRANSIENT |
| 50 | `A:Punching/upgrading HA - in[-] esc | B:Committing/upgrading HA path in[-] -` | 4 | RecvOther×4, DeliverAny×2 | PunchOK→#19×4 | LEGAL-TRANSIENT |
| 51 | `A:Punching/upgrading HA - in[-] esc | B:Punching/upgrading H- - in[-] esc` | 4 | DeliverAny×4 | PunchOK→#40×4, DeliverAny→#8×4 | LEGAL-TRANSIENT |
| 52 | `A:Punching/upgrading HA - in[-] esc | B:Punching/upgrading HA - in[-] -` | 4 | DeliverAny×2 | PunchOK→#42×4, PunchOK→#54×4, Tick→#8×2 | LEGAL-TRANSIENT |
| 53 | `A:Punching/upgrading HA - in[COMPLETE] - | B:Committing/upgrading HA path in[-] esc` | 4 | PunchOK×4 | RecvOther→#48×4, PunchOK→#43×4, Tick→#9×2 | LEGAL-TRANSIENT |
| 54 | `A:Punching/upgrading HA - in[COMPLETE] esc | B:Committing/upgrading HA path in[-] -` | 4 | PunchOK×4, DeliverAny×2 | RecvOther→#50×4, PunchOK→#44×4 | LEGAL-TRANSIENT |
| 55 | `A:AwaitingReady/upgrading -- - in[READY] - | B:Punching/upgrading -- - in[-] -` | 3 | RecvOther×3, ProbeAnswer×1 | RecvOther→#20×3, Tick→#65×1 | LEGAL-TRANSIENT |
| 56 | `A:Failed/relayed -A - in[-] esc | B:Failed/relayed H- - in[-] -` | 3 | RecvOther×3 | - | LEGAL-TERMINAL RelayedOnly |
| 57 | `A:Failed/relayed -A - in[-] esc | B:Punching/upgrading H- - in[REJECT] -` | 3 | PunchFail×2 | RecvOther→#56×3, Tick→#23×1 | LEGAL-TRANSIENT |
| 58 | `A:Failed/relayed H- - in[-] - | B:Failed/relayed -A - in[-] esc` | 3 | RecvOther×3 | - | LEGAL-TERMINAL RelayedOnly |
| 59 | `A:Proposing/upgrading -- - in[ACCEPT,READY] - | B:Punching/upgrading -- - in[-] -` | 3 | ProbeAnswer×1 | RecvOther→#55×3, Tick→#88×1 | LEGAL-TRANSIENT |
| 60 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading -- - in[-] esc` | 3 | RecvOther×2, Tick×1 | DeliverAny→#102×1 | LEGAL-TRANSIENT |
| 61 | `A:Punching/upgrading -- - in[-] esc | B:Punching/upgrading -- - in[-] -` | 3 | Tick×1 | DeliverAny→#107×1 | LEGAL-TRANSIENT |
| 62 | `A:Punching/upgrading -A - in[-] - | B:Punching/upgrading H- - in[-] esc` | 3 | Tick×1, DeliverAny×1 | DeliverAny→#93×2, Tick→#14×1, PunchFail→#104×1 | LEGAL-TRANSIENT |
| 63 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading -A - in[-] -` | 3 | Tick×1, DeliverAny×1 | DeliverAny→#91×2, Tick→#15×1, PunchFail→#97×1 | LEGAL-TRANSIENT |
| 64 | `A:Punching/upgrading H- - in[REJECT] - | B:Failed/relayed -A - in[-] esc` | 3 | PunchFail×2 | RecvOther→#58×3, Tick→#35×1 | LEGAL-TRANSIENT |
| 65 | `A:AwaitingReady/upgrading -- - in[READY] - | B:Punching/upgrading -- - in[-] esc` | 2 | RecvOther×2, Tick×1 | RecvOther→#60×2 | LEGAL-TRANSIENT |
| 66 | `A:Committing/upgrading HA path in[-] - | B:Punching/upgrading H- - in[-] esc` | 2 | RecvOther×2 | DeliverAny→#37×2 | LEGAL-TRANSIENT |
| 67 | `A:Committing/upgrading HA path in[-] - | B:Punching/upgrading H- - in[COMPLETE] esc` | 2 | PunchOK×2 | RecvOther→#66×2, DeliverAny→#38×2 | LEGAL-TRANSIENT |
| 68 | `A:Committing/upgrading HA path in[REJECT] - | B:Failed/relayed H- - in[-] esc` | 2 | RecvOther×2 | RecvOther→#86×2 | LEGAL-TRANSIENT |
| 69 | `A:Committing/upgrading HA path in[REJECT] - | B:Failed/relayed H- - in[COMPLETE] esc` | 2 | PunchOK×2 | RecvOther→#87×2, RecvOther→#68×2 | LEGAL-TRANSIENT |
| 70 | `A:Failed/relayed -A - in[-] - | B:Failed/relayed H- - in[-] esc` | 2 | RecvOther×1 | - | LEGAL-TERMINAL RelayedOnly |
| 71 | `A:Failed/relayed -A - in[-] esc | B:Failed/relayed H- - in[REJECT] esc` | 2 | RecvOther×2 | RecvOther→#12×2 | LEGAL-TRANSIENT |
| 72 | `A:Failed/relayed -A - in[REJECT] esc | B:Failed/relayed H- - in[-] esc` | 2 | RecvOther×2 | RecvOther→#12×2 | LEGAL-TRANSIENT |
| 73 | `A:Failed/relayed -A - in[REJECT] esc | B:Failed/relayed H- - in[REJECT] esc` | 2 | PunchFail×2 | RecvOther→#71×2, RecvOther→#72×2 | LEGAL-TRANSIENT |
| 74 | `A:Failed/relayed H- - in[-] esc | B:Committing/upgrading HA path in[REJECT] -` | 2 | RecvOther×2 | RecvOther→#78×2 | LEGAL-TRANSIENT |
| 75 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed -A - in[-] -` | 2 | RecvOther×1 | - | LEGAL-TERMINAL RelayedOnly |
| 76 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed -A - in[REJECT] esc` | 2 | RecvOther×2 | RecvOther→#13×2 | LEGAL-TRANSIENT |
| 77 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed HA - in[-] -` | 2 | RecvOther×2 | - | LEGAL-TERMINAL RelayedOnly |
| 78 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed HA path in[-] -` | 2 | RecvOther×4 | - | LEGAL-TERMINAL RelayedOnly |
| 79 | `A:Failed/relayed H- - in[-] esc | B:Punching/upgrading -A - in[REJECT] esc` | 2 | PunchFail×2 | RecvOther→#13×2, DeliverAny→#27×2 | LEGAL-TRANSIENT |
| 80 | `A:Failed/relayed H- - in[-] esc | B:Punching/upgrading HA - in[REJECT] -` | 2 | DeliverAny×1 | RecvOther→#77×2, PunchOK→#81×2, Tick→#27×1 | LEGAL-TRANSIENT |
| 81 | `A:Failed/relayed H- - in[COMPLETE] esc | B:Committing/upgrading HA path in[REJECT] -` | 2 | PunchOK×2 | RecvOther→#74×2, RecvOther→#82×2 | LEGAL-TRANSIENT |
| 82 | `A:Failed/relayed H- - in[COMPLETE] esc | B:Failed/relayed HA path in[-] -` | 2 | RecvOther×2 | RecvOther→#78×2 | LEGAL-TRANSIENT |
| 83 | `A:Failed/relayed H- - in[REJECT] esc | B:Failed/relayed -A - in[-] esc` | 2 | RecvOther×2 | RecvOther→#13×2 | LEGAL-TRANSIENT |
| 84 | `A:Failed/relayed H- - in[REJECT] esc | B:Failed/relayed -A - in[REJECT] esc` | 2 | PunchFail×2 | RecvOther→#76×2, RecvOther→#83×2 | LEGAL-TRANSIENT |
| 85 | `A:Failed/relayed HA - in[-] - | B:Failed/relayed H- - in[-] esc` | 2 | RecvOther×2 | - | LEGAL-TERMINAL RelayedOnly |
| 86 | `A:Failed/relayed HA path in[-] - | B:Failed/relayed H- - in[-] esc` | 2 | RecvOther×4 | - | LEGAL-TERMINAL RelayedOnly |
| 87 | `A:Failed/relayed HA path in[-] - | B:Failed/relayed H- - in[COMPLETE] esc` | 2 | RecvOther×2 | RecvOther→#86×2 | LEGAL-TRANSIENT |
| 88 | `A:Proposing/upgrading -- - in[ACCEPT,READY] - | B:Punching/upgrading -- - in[-] esc` | 2 | Tick×1 | RecvOther→#65×2 | LEGAL-TRANSIENT |
| 89 | `A:Punching/upgrading -A - in[REJECT] esc | B:Failed/relayed H- - in[-] esc` | 2 | PunchFail×2 | RecvOther→#12×2, DeliverAny→#36×2 | LEGAL-TRANSIENT |
| 90 | `A:Punching/upgrading H- - in[-] esc | B:Committing/upgrading HA path in[-] -` | 2 | RecvOther×2 | DeliverAny→#50×2 | LEGAL-TRANSIENT |
| 91 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading HA - in[-] -` | 2 | DeliverAny×2 | PunchOK→#92×2, DeliverAny→#52×2 | LEGAL-TRANSIENT |
| 92 | `A:Punching/upgrading H- - in[COMPLETE] esc | B:Committing/upgrading HA path in[-] -` | 2 | PunchOK×2 | RecvOther→#90×2, DeliverAny→#54×2 | LEGAL-TRANSIENT |
| 93 | `A:Punching/upgrading HA - in[-] - | B:Punching/upgrading H- - in[-] esc` | 2 | DeliverAny×2 | PunchOK→#67×2, DeliverAny→#49×2 | LEGAL-TRANSIENT |
| 94 | `A:Punching/upgrading HA - in[REJECT] - | B:Failed/relayed H- - in[-] esc` | 2 | DeliverAny×1 | RecvOther→#85×2, PunchOK→#69×2, Tick→#36×1 | LEGAL-TRANSIENT |
| 95 | `A:AwaitingReady/upgrading -- - in[-] - | B:Discovering/upgrading -- - in[-] -` | 1 | RecvOther×1 | ProbeAnswer→#55×1 | LEGAL-TRANSIENT |
| 96 | `A:Discovering/upgrading -- - in[-] - | B:Idle/relayed -- - in[-] -` | 1 | Propose×1 | ProbeAnswer→#99×1 | LEGAL-TRANSIENT |
| 97 | `A:Failed/relayed H- - in[-] esc | B:Punching/upgrading -A - in[REJECT] -` | 1 | PunchFail×1 | RecvOther→#75×1, DeliverAny→#80×1 | LEGAL-TRANSIENT |
| 98 | `A:Idle/relayed -- - in[-] - | B:Idle/relayed -- - in[-] -` | 1 | - | Propose→#96×1 | LEGAL-TRANSIENT |
| 99 | `A:Proposing/upgrading -- - in[-] - | B:Idle/relayed -- - in[REQUEST] -` | 1 | ProbeAnswer×1 | RecvRequest→#100×1 | LEGAL-TRANSIENT |
| 100 | `A:Proposing/upgrading -- - in[ACCEPT] - | B:Discovering/upgrading -- - in[-] -` | 1 | RecvRequest×1 | RecvOther→#95×1, ProbeAnswer→#59×1 | LEGAL-TRANSIENT |
| 101 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading H- - in[-] -` | 1 | DeliverAny×1 | DeliverAny→#103×1 | LEGAL-TRANSIENT |
| 102 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading H- - in[-] esc` | 1 | DeliverAny×1 | DeliverAny→#62×1 | LEGAL-TRANSIENT |
| 103 | `A:Punching/upgrading -A - in[-] - | B:Punching/upgrading H- - in[-] -` | 1 | DeliverAny×1 | Tick→#33×1, Tick→#62×1 | LEGAL-TRANSIENT |
| 104 | `A:Punching/upgrading -A - in[REJECT] - | B:Failed/relayed H- - in[-] esc` | 1 | PunchFail×1 | RecvOther→#70×1, DeliverAny→#94×1 | LEGAL-TRANSIENT |
| 105 | `A:Punching/upgrading H- - in[-] - | B:Punching/upgrading -- - in[-] -` | 1 | DeliverAny×1 | DeliverAny→#106×1 | LEGAL-TRANSIENT |
| 106 | `A:Punching/upgrading H- - in[-] - | B:Punching/upgrading -A - in[-] -` | 1 | DeliverAny×1 | Tick→#63×1, Tick→#34×1 | LEGAL-TRANSIENT |
| 107 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading -- - in[-] -` | 1 | DeliverAny×1 | DeliverAny→#63×1 | LEGAL-TRANSIENT |

Classes with no path to a terminal state, or a rejected terminal: 0.
