### State classes: teardown

1141 states, 2054 transitions, 144 classes. Class: per side `phase/linkPath heard-acked path in[signals in flight] flags`.

| # | class (A \| B) | states | in (by action) | out (action -> class) | verdict |
|---|---|---|---|---|---|
| 1 | `A:Connected/direct HA path in[-] esc | B:Connected/direct HA path in[-] esc` | 60 | RecvOther×80 | TearDown→#2×60 | LEGAL-TRANSIENT |
| 2 | `A:Gone/none HA path in[-] esc | B:Gone/none HA path in[-] esc` | 60 | TearDown×160 | - | LEGAL-TERMINAL LinkClosed |
| 3 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Connected/direct HA path in[-] esc` | 40 | PunchOK×20, RecvOther×20 | RecvOther→#1×40, TearDown→#2×40 | LEGAL-TRANSIENT |
| 4 | `A:Connected/direct HA path in[-] esc | B:Committing/upgrading HA path in[COMPLETE] esc` | 40 | RecvOther×20, PunchOK×20 | RecvOther→#1×40, TearDown→#2×40 | LEGAL-TRANSIENT |
| 5 | `A:Gone/none HA - in[-] esc | B:Gone/none HA path in[-] esc` | 40 | TearDown×40 | - | LEGAL-TERMINAL LinkClosed |
| 6 | `A:Gone/none HA path in[-] esc | B:Gone/none HA - in[-] esc` | 40 | TearDown×40 | - | LEGAL-TERMINAL LinkClosed |
| 7 | `A:Gone/none -A - in[-] esc | B:Gone/none H- - in[-] esc` | 34 | TearDown×36 | - | LEGAL-TERMINAL LinkClosed |
| 8 | `A:Gone/none H- - in[-] esc | B:Gone/none -A - in[-] esc` | 34 | TearDown×36 | - | LEGAL-TERMINAL LinkClosed |
| 9 | `A:Gone/none H- - in[-] esc | B:Gone/none HA path in[-] esc` | 28 | TearDown×32 | - | LEGAL-TERMINAL LinkClosed |
| 10 | `A:Gone/none HA path in[-] esc | B:Gone/none H- - in[-] esc` | 28 | TearDown×32 | - | LEGAL-TERMINAL LinkClosed |
| 11 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading HA - in[-] esc` | 20 | RecvOther×20, DeliverAny×4, Tick×2 | PunchOK→#3×20, TearDown→#6×20 | LEGAL-TRANSIENT |
| 12 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading HA - in[COMPLETE] esc` | 20 | PunchOK×20, DeliverAny×4, Tick×2 | RecvOther→#11×20, PunchOK→#13×20, TearDown→#6×20 | LEGAL-TRANSIENT |
| 13 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Committing/upgrading HA path in[COMPLETE] esc` | 20 | PunchOK×40 | RecvOther→#4×20, RecvOther→#3×20, TearDown→#2×20 | LEGAL-TRANSIENT |
| 14 | `A:Gone/none H- - in[-] esc | B:Gone/none HA - in[-] esc` | 20 | TearDown×16 | - | LEGAL-TERMINAL LinkClosed |
| 15 | `A:Gone/none HA - in[-] esc | B:Gone/none H- - in[-] esc` | 20 | TearDown×16 | - | LEGAL-TERMINAL LinkClosed |
| 16 | `A:Gone/none HA - in[-] esc | B:Gone/none HA - in[-] esc` | 20 | TearDown×20 | - | LEGAL-TERMINAL LinkClosed |
| 17 | `A:Punching/upgrading HA - in[-] esc | B:Committing/upgrading HA path in[-] esc` | 20 | RecvOther×20, DeliverAny×4, Tick×2 | PunchOK→#4×20, TearDown→#5×20 | LEGAL-TRANSIENT |
| 18 | `A:Punching/upgrading HA - in[-] esc | B:Punching/upgrading HA - in[-] esc` | 20 | DeliverAny×8, Tick×4 | PunchOK→#12×20, PunchOK→#19×20, TearDown→#16×20 | LEGAL-TRANSIENT |
| 19 | `A:Punching/upgrading HA - in[COMPLETE] esc | B:Committing/upgrading HA path in[-] esc` | 20 | PunchOK×20, DeliverAny×4, Tick×2 | RecvOther→#17×20, PunchOK→#13×20, TearDown→#5×20 | LEGAL-TRANSIENT |
| 20 | `A:Connected/direct HA path in[-] - | B:Connected/direct HA path in[-] esc` | 12 | RecvOther×16 | TearDown→#29×12 | LEGAL-TRANSIENT |
| 21 | `A:Connected/direct HA path in[-] esc | B:Connected/direct HA path in[-] -` | 12 | RecvOther×16 | TearDown→#30×12 | LEGAL-TRANSIENT |
| 22 | `A:Failed/relayed -A - in[-] esc | B:Failed/relayed H- - in[-] esc` | 12 | RecvOther×12 | TearDown→#7×12 | LEGAL-TRANSIENT |
| 23 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed -A - in[-] esc` | 12 | RecvOther×12 | TearDown→#8×12 | LEGAL-TRANSIENT |
| 24 | `A:Gone/none -- - in[-] - | B:Gone/none -- - in[-] -` | 12 | TearDown×15 | - | LEGAL-TERMINAL LinkClosed |
| 25 | `A:Gone/none -A - in[-] esc | B:Gone/none H- - in[-] -` | 12 | TearDown×12 | - | LEGAL-TERMINAL LinkClosed |
| 26 | `A:Gone/none H- - in[-] - | B:Gone/none -A - in[-] esc` | 12 | TearDown×12 | - | LEGAL-TERMINAL LinkClosed |
| 27 | `A:Gone/none H- - in[-] esc | B:Gone/none HA path in[-] -` | 12 | TearDown×12 | - | LEGAL-TERMINAL LinkClosed |
| 28 | `A:Gone/none HA path in[-] - | B:Gone/none H- - in[-] esc` | 12 | TearDown×12 | - | LEGAL-TERMINAL LinkClosed |
| 29 | `A:Gone/none HA path in[-] - | B:Gone/none HA path in[-] esc` | 12 | TearDown×32 | - | LEGAL-TERMINAL LinkClosed |
| 30 | `A:Gone/none HA path in[-] esc | B:Gone/none HA path in[-] -` | 12 | TearDown×32 | - | LEGAL-TERMINAL LinkClosed |
| 31 | `A:Punching/upgrading -A - in[-] esc | B:Punching/upgrading H- - in[-] esc` | 10 | Tick×3 | TearDown→#7×10, DeliverAny→#80×4, PunchFail→#48×2, PunchFail→#123×2 | LEGAL-TRANSIENT |
| 32 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading -A - in[-] esc` | 10 | Tick×3 | TearDown→#8×10, DeliverAny→#75×4, PunchFail→#108×2, PunchFail→#61×2 | LEGAL-TRANSIENT |
| 33 | `A:Committing/upgrading HA path in[COMPLETE] - | B:Connected/direct HA path in[-] esc` | 8 | PunchOK×4, RecvOther×4 | RecvOther→#20×8, TearDown→#29×8 | LEGAL-TRANSIENT |
| 34 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Connected/direct HA path in[-] -` | 8 | RecvOther×4, PunchOK×4 | RecvOther→#21×8, TearDown→#30×8 | LEGAL-TRANSIENT |
| 35 | `A:Connected/direct HA path in[-] - | B:Committing/upgrading HA path in[COMPLETE] esc` | 8 | RecvOther×4, PunchOK×4 | RecvOther→#20×8, TearDown→#29×8 | LEGAL-TRANSIENT |
| 36 | `A:Connected/direct HA path in[-] esc | B:Committing/upgrading HA path in[COMPLETE] -` | 8 | PunchOK×4, RecvOther×4 | RecvOther→#21×8, TearDown→#30×8 | LEGAL-TRANSIENT |
| 37 | `A:Gone/none -A - in[-] - | B:Gone/none H- - in[-] esc` | 8 | TearDown×6 | - | LEGAL-TERMINAL LinkClosed |
| 38 | `A:Gone/none H- - in[-] esc | B:Gone/none -A - in[-] -` | 8 | TearDown×6 | - | LEGAL-TERMINAL LinkClosed |
| 39 | `A:Gone/none H- - in[-] esc | B:Gone/none HA - in[-] -` | 8 | TearDown×6 | - | LEGAL-TERMINAL LinkClosed |
| 40 | `A:Gone/none HA - in[-] - | B:Gone/none H- - in[-] esc` | 8 | TearDown×6 | - | LEGAL-TERMINAL LinkClosed |
| 41 | `A:Gone/none HA - in[-] - | B:Gone/none HA path in[-] esc` | 8 | TearDown×8 | - | LEGAL-TERMINAL LinkClosed |
| 42 | `A:Gone/none HA - in[-] esc | B:Gone/none HA path in[-] -` | 8 | TearDown×8 | - | LEGAL-TERMINAL LinkClosed |
| 43 | `A:Gone/none HA path in[-] - | B:Gone/none HA - in[-] esc` | 8 | TearDown×8 | - | LEGAL-TERMINAL LinkClosed |
| 44 | `A:Gone/none HA path in[-] esc | B:Gone/none HA - in[-] -` | 8 | TearDown×8 | - | LEGAL-TERMINAL LinkClosed |
| 45 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading -- - in[-] -` | 7 | RecvOther×3 | TearDown→#24×7, Tick→#90×1, Tick→#89×1, DeliverAny→#142×1, DeliverAny→#138×1 | LEGAL-TRANSIENT |
| 46 | `A:Committing/upgrading HA path in[REJECT] esc | B:Failed/relayed H- - in[-] esc` | 6 | RecvOther×6 | RecvOther→#56×6, TearDown→#10×6 | LEGAL-TRANSIENT |
| 47 | `A:Committing/upgrading HA path in[REJECT] esc | B:Failed/relayed H- - in[COMPLETE] esc` | 6 | PunchOK×6 | RecvOther→#57×6, RecvOther→#46×6, TearDown→#10×6 | LEGAL-TRANSIENT |
| 48 | `A:Failed/relayed -A - in[-] esc | B:Punching/upgrading H- - in[REJECT] esc` | 6 | PunchFail×2, Tick×1 | RecvOther→#22×6, TearDown→#7×6, PunchFail→#102×2 | LEGAL-TRANSIENT |
| 49 | `A:Failed/relayed H- - in[-] esc | B:Committing/upgrading HA path in[REJECT] esc` | 6 | RecvOther×6 | RecvOther→#51×6, TearDown→#9×6 | LEGAL-TRANSIENT |
| 50 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed HA - in[-] esc` | 6 | RecvOther×6 | TearDown→#14×6 | LEGAL-TRANSIENT |
| 51 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed HA path in[-] esc` | 6 | RecvOther×12 | TearDown→#9×6 | LEGAL-TRANSIENT |
| 52 | `A:Failed/relayed H- - in[-] esc | B:Punching/upgrading HA - in[REJECT] esc` | 6 | DeliverAny×2, Tick×1 | RecvOther→#50×6, PunchOK→#53×6, TearDown→#14×6 | LEGAL-TRANSIENT |
| 53 | `A:Failed/relayed H- - in[COMPLETE] esc | B:Committing/upgrading HA path in[REJECT] esc` | 6 | PunchOK×6 | RecvOther→#49×6, RecvOther→#54×6, TearDown→#9×6 | LEGAL-TRANSIENT |
| 54 | `A:Failed/relayed H- - in[COMPLETE] esc | B:Failed/relayed HA path in[-] esc` | 6 | RecvOther×6 | RecvOther→#51×6, TearDown→#9×6 | LEGAL-TRANSIENT |
| 55 | `A:Failed/relayed HA - in[-] esc | B:Failed/relayed H- - in[-] esc` | 6 | RecvOther×6 | TearDown→#15×6 | LEGAL-TRANSIENT |
| 56 | `A:Failed/relayed HA path in[-] esc | B:Failed/relayed H- - in[-] esc` | 6 | RecvOther×12 | TearDown→#10×6 | LEGAL-TRANSIENT |
| 57 | `A:Failed/relayed HA path in[-] esc | B:Failed/relayed H- - in[COMPLETE] esc` | 6 | RecvOther×6 | RecvOther→#56×6, TearDown→#10×6 | LEGAL-TRANSIENT |
| 58 | `A:Gone/none -- - in[-] - | B:Gone/none -- - in[-] esc` | 6 | TearDown×7 | - | LEGAL-TERMINAL LinkClosed |
| 59 | `A:Punching/upgrading -A - in[-] esc | B:Punching/upgrading H- - in[-] -` | 6 | Tick×1 | TearDown→#25×6, Tick→#31×2, PunchFail→#86×2 | LEGAL-TRANSIENT |
| 60 | `A:Punching/upgrading H- - in[-] - | B:Punching/upgrading -A - in[-] esc` | 6 | Tick×1 | TearDown→#26×6, Tick→#32×2, PunchFail→#93×2 | LEGAL-TRANSIENT |
| 61 | `A:Punching/upgrading H- - in[REJECT] esc | B:Failed/relayed -A - in[-] esc` | 6 | PunchFail×2, Tick×1 | RecvOther→#23×6, TearDown→#8×6, PunchFail→#113×2 | LEGAL-TRANSIENT |
| 62 | `A:Punching/upgrading HA - in[REJECT] esc | B:Failed/relayed H- - in[-] esc` | 6 | DeliverAny×2, Tick×1 | RecvOther→#55×6, PunchOK→#47×6, TearDown→#15×6 | LEGAL-TRANSIENT |
| 63 | `A:Committing/upgrading HA path in[-] - | B:Punching/upgrading HA - in[-] esc` | 4 | RecvOther×4, DeliverAny×2 | PunchOK→#33×4, TearDown→#43×4 | LEGAL-TRANSIENT |
| 64 | `A:Committing/upgrading HA path in[-] - | B:Punching/upgrading HA - in[COMPLETE] esc` | 4 | PunchOK×4, DeliverAny×2 | RecvOther→#63×4, PunchOK→#69×4, TearDown→#43×4 | LEGAL-TRANSIENT |
| 65 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading H- - in[-] esc` | 4 | RecvOther×4 | DeliverAny→#11×4, TearDown→#10×4 | LEGAL-TRANSIENT |
| 66 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading H- - in[COMPLETE] esc` | 4 | PunchOK×4 | RecvOther→#65×4, DeliverAny→#12×4, TearDown→#10×4 | LEGAL-TRANSIENT |
| 67 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading HA - in[-] -` | 4 | RecvOther×4 | PunchOK→#34×4, TearDown→#44×4, Tick→#11×2 | LEGAL-TRANSIENT |
| 68 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading HA - in[COMPLETE] -` | 4 | PunchOK×4 | RecvOther→#67×4, PunchOK→#70×4, TearDown→#44×4, Tick→#12×2 | LEGAL-TRANSIENT |
| 69 | `A:Committing/upgrading HA path in[COMPLETE] - | B:Committing/upgrading HA path in[COMPLETE] esc` | 4 | PunchOK×8 | RecvOther→#35×4, RecvOther→#33×4, TearDown→#29×4 | LEGAL-TRANSIENT |
| 70 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Committing/upgrading HA path in[COMPLETE] -` | 4 | PunchOK×8 | RecvOther→#36×4, RecvOther→#34×4, TearDown→#30×4 | LEGAL-TRANSIENT |
| 71 | `A:Gone/none -- - in[-] esc | B:Gone/none -- - in[-] -` | 4 | TearDown×3 | - | LEGAL-TERMINAL LinkClosed |
| 72 | `A:Gone/none HA - in[-] - | B:Gone/none HA - in[-] esc` | 4 | TearDown×4 | - | LEGAL-TERMINAL LinkClosed |
| 73 | `A:Gone/none HA - in[-] esc | B:Gone/none HA - in[-] -` | 4 | TearDown×4 | - | LEGAL-TERMINAL LinkClosed |
| 74 | `A:Punching/upgrading H- - in[-] esc | B:Committing/upgrading HA path in[-] esc` | 4 | RecvOther×4 | DeliverAny→#17×4, TearDown→#9×4 | LEGAL-TRANSIENT |
| 75 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading HA - in[-] esc` | 4 | DeliverAny×4 | PunchOK→#76×4, DeliverAny→#18×4, TearDown→#14×4 | LEGAL-TRANSIENT |
| 76 | `A:Punching/upgrading H- - in[COMPLETE] esc | B:Committing/upgrading HA path in[-] esc` | 4 | PunchOK×4 | RecvOther→#74×4, DeliverAny→#19×4, TearDown→#9×4 | LEGAL-TRANSIENT |
| 77 | `A:Punching/upgrading HA - in[-] - | B:Committing/upgrading HA path in[-] esc` | 4 | RecvOther×4 | PunchOK→#35×4, TearDown→#41×4, Tick→#17×2 | LEGAL-TRANSIENT |
| 78 | `A:Punching/upgrading HA - in[-] - | B:Punching/upgrading HA - in[-] esc` | 4 | DeliverAny×2 | PunchOK→#64×4, PunchOK→#82×4, TearDown→#72×4, Tick→#18×2 | LEGAL-TRANSIENT |
| 79 | `A:Punching/upgrading HA - in[-] esc | B:Committing/upgrading HA path in[-] -` | 4 | RecvOther×4, DeliverAny×2 | PunchOK→#36×4, TearDown→#42×4 | LEGAL-TRANSIENT |
| 80 | `A:Punching/upgrading HA - in[-] esc | B:Punching/upgrading H- - in[-] esc` | 4 | DeliverAny×4 | PunchOK→#66×4, DeliverAny→#18×4, TearDown→#15×4 | LEGAL-TRANSIENT |
| 81 | `A:Punching/upgrading HA - in[-] esc | B:Punching/upgrading HA - in[-] -` | 4 | DeliverAny×2 | PunchOK→#68×4, PunchOK→#83×4, TearDown→#73×4, Tick→#18×2 | LEGAL-TRANSIENT |
| 82 | `A:Punching/upgrading HA - in[COMPLETE] - | B:Committing/upgrading HA path in[-] esc` | 4 | PunchOK×4 | RecvOther→#77×4, PunchOK→#69×4, TearDown→#41×4, Tick→#19×2 | LEGAL-TRANSIENT |
| 83 | `A:Punching/upgrading HA - in[COMPLETE] esc | B:Committing/upgrading HA path in[-] -` | 4 | PunchOK×4, DeliverAny×2 | RecvOther→#79×4, PunchOK→#70×4, TearDown→#42×4 | LEGAL-TRANSIENT |
| 84 | `A:AwaitingReady/upgrading -- - in[READY] - | B:Punching/upgrading -- - in[-] -` | 3 | RecvOther×3, ProbeAnswer×1 | RecvOther→#45×3, TearDown→#24×3, Tick→#94×1 | LEGAL-TRANSIENT |
| 85 | `A:Failed/relayed -A - in[-] esc | B:Failed/relayed H- - in[-] -` | 3 | RecvOther×3 | TearDown→#25×3 | LEGAL-TRANSIENT |
| 86 | `A:Failed/relayed -A - in[-] esc | B:Punching/upgrading H- - in[REJECT] -` | 3 | PunchFail×2 | RecvOther→#85×3, TearDown→#25×3, Tick→#48×1 | LEGAL-TRANSIENT |
| 87 | `A:Failed/relayed H- - in[-] - | B:Failed/relayed -A - in[-] esc` | 3 | RecvOther×3 | TearDown→#26×3 | LEGAL-TRANSIENT |
| 88 | `A:Proposing/upgrading -- - in[ACCEPT,READY] - | B:Punching/upgrading -- - in[-] -` | 3 | ProbeAnswer×1 | RecvOther→#84×3, TearDown→#24×3, Tick→#122×1 | LEGAL-TRANSIENT |
| 89 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading -- - in[-] esc` | 3 | RecvOther×2, Tick×1 | TearDown→#58×3, DeliverAny→#139×1 | LEGAL-TRANSIENT |
| 90 | `A:Punching/upgrading -- - in[-] esc | B:Punching/upgrading -- - in[-] -` | 3 | Tick×1 | TearDown→#71×3, DeliverAny→#144×1 | LEGAL-TRANSIENT |
| 91 | `A:Punching/upgrading -A - in[-] - | B:Punching/upgrading H- - in[-] esc` | 3 | Tick×1, DeliverAny×1 | TearDown→#37×3, DeliverAny→#127×2, Tick→#31×1, PunchFail→#141×1 | LEGAL-TRANSIENT |
| 92 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading -A - in[-] -` | 3 | Tick×1, DeliverAny×1 | TearDown→#38×3, DeliverAny→#125×2, Tick→#32×1, PunchFail→#131×1 | LEGAL-TRANSIENT |
| 93 | `A:Punching/upgrading H- - in[REJECT] - | B:Failed/relayed -A - in[-] esc` | 3 | PunchFail×2 | RecvOther→#87×3, TearDown→#26×3, Tick→#61×1 | LEGAL-TRANSIENT |
| 94 | `A:AwaitingReady/upgrading -- - in[READY] - | B:Punching/upgrading -- - in[-] esc` | 2 | RecvOther×2, Tick×1 | RecvOther→#89×2, TearDown→#58×2 | LEGAL-TRANSIENT |
| 95 | `A:Committing/upgrading HA path in[-] - | B:Punching/upgrading H- - in[-] esc` | 2 | RecvOther×2 | DeliverAny→#63×2, TearDown→#28×2 | LEGAL-TRANSIENT |
| 96 | `A:Committing/upgrading HA path in[-] - | B:Punching/upgrading H- - in[COMPLETE] esc` | 2 | PunchOK×2 | RecvOther→#95×2, DeliverAny→#64×2, TearDown→#28×2 | LEGAL-TRANSIENT |
| 97 | `A:Committing/upgrading HA path in[REJECT] - | B:Failed/relayed H- - in[-] esc` | 2 | RecvOther×2 | RecvOther→#115×2, TearDown→#28×2 | LEGAL-TRANSIENT |
| 98 | `A:Committing/upgrading HA path in[REJECT] - | B:Failed/relayed H- - in[COMPLETE] esc` | 2 | PunchOK×2 | RecvOther→#116×2, RecvOther→#97×2, TearDown→#28×2 | LEGAL-TRANSIENT |
| 99 | `A:Failed/relayed -A - in[-] - | B:Failed/relayed H- - in[-] esc` | 2 | RecvOther×1 | TearDown→#37×2 | LEGAL-TRANSIENT |
| 100 | `A:Failed/relayed -A - in[-] esc | B:Failed/relayed H- - in[REJECT] esc` | 2 | RecvOther×2 | RecvOther→#22×2, TearDown→#7×2 | LEGAL-TRANSIENT |
| 101 | `A:Failed/relayed -A - in[REJECT] esc | B:Failed/relayed H- - in[-] esc` | 2 | RecvOther×2 | RecvOther→#22×2, TearDown→#7×2 | LEGAL-TRANSIENT |
| 102 | `A:Failed/relayed -A - in[REJECT] esc | B:Failed/relayed H- - in[REJECT] esc` | 2 | PunchFail×2 | RecvOther→#100×2, RecvOther→#101×2, TearDown→#7×2 | LEGAL-TRANSIENT |
| 103 | `A:Failed/relayed H- - in[-] esc | B:Committing/upgrading HA path in[REJECT] -` | 2 | RecvOther×2 | RecvOther→#107×2, TearDown→#27×2 | LEGAL-TRANSIENT |
| 104 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed -A - in[-] -` | 2 | RecvOther×1 | TearDown→#38×2 | LEGAL-TRANSIENT |
| 105 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed -A - in[REJECT] esc` | 2 | RecvOther×2 | RecvOther→#23×2, TearDown→#8×2 | LEGAL-TRANSIENT |
| 106 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed HA - in[-] -` | 2 | RecvOther×2 | TearDown→#39×2 | LEGAL-TRANSIENT |
| 107 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed HA path in[-] -` | 2 | RecvOther×4 | TearDown→#27×2 | LEGAL-TRANSIENT |
| 108 | `A:Failed/relayed H- - in[-] esc | B:Punching/upgrading -A - in[REJECT] esc` | 2 | PunchFail×2 | RecvOther→#23×2, DeliverAny→#52×2, TearDown→#8×2 | LEGAL-TRANSIENT |
| 109 | `A:Failed/relayed H- - in[-] esc | B:Punching/upgrading HA - in[REJECT] -` | 2 | DeliverAny×1 | RecvOther→#106×2, PunchOK→#110×2, TearDown→#39×2, Tick→#52×1 | LEGAL-TRANSIENT |
| 110 | `A:Failed/relayed H- - in[COMPLETE] esc | B:Committing/upgrading HA path in[REJECT] -` | 2 | PunchOK×2 | RecvOther→#103×2, RecvOther→#111×2, TearDown→#27×2 | LEGAL-TRANSIENT |
| 111 | `A:Failed/relayed H- - in[COMPLETE] esc | B:Failed/relayed HA path in[-] -` | 2 | RecvOther×2 | RecvOther→#107×2, TearDown→#27×2 | LEGAL-TRANSIENT |
| 112 | `A:Failed/relayed H- - in[REJECT] esc | B:Failed/relayed -A - in[-] esc` | 2 | RecvOther×2 | RecvOther→#23×2, TearDown→#8×2 | LEGAL-TRANSIENT |
| 113 | `A:Failed/relayed H- - in[REJECT] esc | B:Failed/relayed -A - in[REJECT] esc` | 2 | PunchFail×2 | RecvOther→#105×2, RecvOther→#112×2, TearDown→#8×2 | LEGAL-TRANSIENT |
| 114 | `A:Failed/relayed HA - in[-] - | B:Failed/relayed H- - in[-] esc` | 2 | RecvOther×2 | TearDown→#40×2 | LEGAL-TRANSIENT |
| 115 | `A:Failed/relayed HA path in[-] - | B:Failed/relayed H- - in[-] esc` | 2 | RecvOther×4 | TearDown→#28×2 | LEGAL-TRANSIENT |
| 116 | `A:Failed/relayed HA path in[-] - | B:Failed/relayed H- - in[COMPLETE] esc` | 2 | RecvOther×2 | RecvOther→#115×2, TearDown→#28×2 | LEGAL-TRANSIENT |
| 117 | `A:Gone/none -- - in[-] - | B:Gone/none H- - in[-] -` | 2 | TearDown×1 | - | LEGAL-TERMINAL LinkClosed |
| 118 | `A:Gone/none -- - in[-] - | B:Gone/none H- - in[-] esc` | 2 | TearDown×1 | - | LEGAL-TERMINAL LinkClosed |
| 119 | `A:Gone/none -- - in[-] - | B:Idle/none -- - in[-] -` | 2 | TearDown×2 | - | LEGAL-TERMINAL LinkClosed |
| 120 | `A:Gone/none H- - in[-] - | B:Gone/none -- - in[-] -` | 2 | TearDown×1 | - | LEGAL-TERMINAL LinkClosed |
| 121 | `A:Gone/none H- - in[-] esc | B:Gone/none -- - in[-] -` | 2 | TearDown×1 | - | LEGAL-TERMINAL LinkClosed |
| 122 | `A:Proposing/upgrading -- - in[ACCEPT,READY] - | B:Punching/upgrading -- - in[-] esc` | 2 | Tick×1 | RecvOther→#94×2, TearDown→#58×2 | LEGAL-TRANSIENT |
| 123 | `A:Punching/upgrading -A - in[REJECT] esc | B:Failed/relayed H- - in[-] esc` | 2 | PunchFail×2 | RecvOther→#22×2, DeliverAny→#62×2, TearDown→#7×2 | LEGAL-TRANSIENT |
| 124 | `A:Punching/upgrading H- - in[-] esc | B:Committing/upgrading HA path in[-] -` | 2 | RecvOther×2 | DeliverAny→#79×2, TearDown→#27×2 | LEGAL-TRANSIENT |
| 125 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading HA - in[-] -` | 2 | DeliverAny×2 | PunchOK→#126×2, DeliverAny→#81×2, TearDown→#39×2 | LEGAL-TRANSIENT |
| 126 | `A:Punching/upgrading H- - in[COMPLETE] esc | B:Committing/upgrading HA path in[-] -` | 2 | PunchOK×2 | RecvOther→#124×2, DeliverAny→#83×2, TearDown→#27×2 | LEGAL-TRANSIENT |
| 127 | `A:Punching/upgrading HA - in[-] - | B:Punching/upgrading H- - in[-] esc` | 2 | DeliverAny×2 | PunchOK→#96×2, DeliverAny→#78×2, TearDown→#40×2 | LEGAL-TRANSIENT |
| 128 | `A:Punching/upgrading HA - in[REJECT] - | B:Failed/relayed H- - in[-] esc` | 2 | DeliverAny×1 | RecvOther→#114×2, PunchOK→#98×2, TearDown→#40×2, Tick→#62×1 | LEGAL-TRANSIENT |
| 129 | `A:AwaitingReady/upgrading -- - in[-] - | B:Discovering/upgrading -- - in[-] -` | 1 | RecvOther×1 | ProbeAnswer→#84×1, TearDown→#24×1 | LEGAL-TRANSIENT |
| 130 | `A:Discovering/upgrading -- - in[-] - | B:Idle/relayed -- - in[-] -` | 1 | Propose×1 | ProbeAnswer→#136×1, TearDown→#119×1 | LEGAL-TRANSIENT |
| 131 | `A:Failed/relayed H- - in[-] esc | B:Punching/upgrading -A - in[REJECT] -` | 1 | PunchFail×1 | RecvOther→#104×1, DeliverAny→#109×1, TearDown→#38×1 | LEGAL-TRANSIENT |
| 132 | `A:Gone/none -A - in[-] - | B:Gone/none H- - in[-] -` | 1 | TearDown×1 | - | LEGAL-TERMINAL LinkClosed |
| 133 | `A:Gone/none H- - in[-] - | B:Gone/none -A - in[-] -` | 1 | TearDown×1 | - | LEGAL-TERMINAL LinkClosed |
| 134 | `A:Idle/none -- - in[-] - | B:Idle/none -- - in[-] -` | 1 | TearDown×1 | - | LEGAL-TERMINAL LinkClosed |
| 135 | `A:Idle/relayed -- - in[-] - | B:Idle/relayed -- - in[-] -` | 1 | - | Propose→#130×1, TearDown→#134×1 | LEGAL-TRANSIENT |
| 136 | `A:Proposing/upgrading -- - in[-] - | B:Idle/relayed -- - in[REQUEST] -` | 1 | ProbeAnswer×1 | RecvRequest→#137×1, TearDown→#119×1 | LEGAL-TRANSIENT |
| 137 | `A:Proposing/upgrading -- - in[ACCEPT] - | B:Discovering/upgrading -- - in[-] -` | 1 | RecvRequest×1 | RecvOther→#129×1, ProbeAnswer→#88×1, TearDown→#24×1 | LEGAL-TRANSIENT |
| 138 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading H- - in[-] -` | 1 | DeliverAny×1 | DeliverAny→#140×1, TearDown→#117×1 | LEGAL-TRANSIENT |
| 139 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading H- - in[-] esc` | 1 | DeliverAny×1 | DeliverAny→#91×1, TearDown→#118×1 | LEGAL-TRANSIENT |
| 140 | `A:Punching/upgrading -A - in[-] - | B:Punching/upgrading H- - in[-] -` | 1 | DeliverAny×1 | Tick→#59×1, Tick→#91×1, TearDown→#132×1 | LEGAL-TRANSIENT |
| 141 | `A:Punching/upgrading -A - in[REJECT] - | B:Failed/relayed H- - in[-] esc` | 1 | PunchFail×1 | RecvOther→#99×1, DeliverAny→#128×1, TearDown→#37×1 | LEGAL-TRANSIENT |
| 142 | `A:Punching/upgrading H- - in[-] - | B:Punching/upgrading -- - in[-] -` | 1 | DeliverAny×1 | DeliverAny→#143×1, TearDown→#120×1 | LEGAL-TRANSIENT |
| 143 | `A:Punching/upgrading H- - in[-] - | B:Punching/upgrading -A - in[-] -` | 1 | DeliverAny×1 | Tick→#92×1, Tick→#60×1, TearDown→#133×1 | LEGAL-TRANSIENT |
| 144 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading -- - in[-] -` | 1 | DeliverAny×1 | DeliverAny→#92×1, TearDown→#121×1 | LEGAL-TRANSIENT |

Classes with no path to a terminal state, or a rejected terminal: 0.
