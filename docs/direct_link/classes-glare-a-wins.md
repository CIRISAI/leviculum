### State classes: glare-a-wins

2189 states, 4174 transitions, 202 classes. Class: per side `phase/linkPath heard-acked path in[signals in flight] flags`.

| # | class (A \| B) | states | in (by action) | out (action -> class) | verdict |
|---|---|---|---|---|---|
| 1 | `A:Connected/direct HA path in[-] esc | B:Connected/direct HA path in[-] esc` | 180 | RecvOther×240 | - | LEGAL-TERMINAL DirectPathUp |
| 2 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Connected/direct HA path in[-] esc` | 120 | PunchOK×60, RecvOther×60 | RecvOther→#1×120 | LEGAL-TRANSIENT |
| 3 | `A:Connected/direct HA path in[-] esc | B:Committing/upgrading HA path in[COMPLETE] esc` | 120 | RecvOther×100, PunchOK×60 | RecvOther→#1×120 | LEGAL-TRANSIENT |
| 4 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading HA - in[-] esc` | 60 | RecvOther×60, DeliverAny×12, Tick×6 | PunchOK→#2×60 | LEGAL-TRANSIENT |
| 5 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading HA - in[COMPLETE] esc` | 60 | PunchOK×60, RecvOther×20, DeliverAny×12, Tick×6 | RecvOther→#4×60, PunchOK→#6×60 | LEGAL-TRANSIENT |
| 6 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Committing/upgrading HA path in[COMPLETE] esc` | 60 | PunchOK×120, RecvOther×20 | RecvOther→#3×60, RecvOther→#2×60 | LEGAL-TRANSIENT |
| 7 | `A:Punching/upgrading HA - in[-] esc | B:Committing/upgrading HA path in[-] esc` | 60 | RecvOther×80, DeliverAny×12, Tick×6 | PunchOK→#3×60 | LEGAL-TRANSIENT |
| 8 | `A:Punching/upgrading HA - in[-] esc | B:Punching/upgrading HA - in[-] esc` | 60 | DeliverAny×24, RecvOther×20, Tick×12 | PunchOK→#5×60, PunchOK→#9×60 | LEGAL-TRANSIENT |
| 9 | `A:Punching/upgrading HA - in[COMPLETE] esc | B:Committing/upgrading HA path in[-] esc` | 60 | PunchOK×60, RecvOther×20, DeliverAny×12, Tick×6 | RecvOther→#7×60, PunchOK→#6×60 | LEGAL-TRANSIENT |
| 10 | `A:Connected/direct HA path in[-] esc | B:Committing/upgrading HA path in[COMPLETE,REJECT] esc` | 40 | RecvOther×20, PunchOK×20 | RecvOther→#3×40 | LEGAL-TRANSIENT |
| 11 | `A:Connected/direct HA path in[-] - | B:Connected/direct HA path in[-] esc` | 36 | RecvOther×48 | - | LEGAL-TERMINAL DirectPathUp |
| 12 | `A:Connected/direct HA path in[-] esc | B:Connected/direct HA path in[-] -` | 36 | RecvOther×48 | - | LEGAL-TERMINAL DirectPathUp |
| 13 | `A:Punching/upgrading -A - in[-] esc | B:Punching/upgrading H- - in[-] esc` | 30 | RecvOther×10, Tick×9 | DeliverAny→#57×12, PunchFail→#27×6, PunchFail→#119×6 | LEGAL-TRANSIENT |
| 14 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading -A - in[-] esc` | 30 | RecvOther×10, Tick×9 | DeliverAny→#51×12, PunchFail→#81×6, PunchFail→#53×6 | LEGAL-TRANSIENT |
| 15 | `A:Committing/upgrading HA path in[COMPLETE] - | B:Connected/direct HA path in[-] esc` | 24 | PunchOK×12, RecvOther×12 | RecvOther→#11×24 | LEGAL-TRANSIENT |
| 16 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Connected/direct HA path in[-] -` | 24 | RecvOther×12, PunchOK×12 | RecvOther→#12×24 | LEGAL-TRANSIENT |
| 17 | `A:Connected/direct HA path in[-] - | B:Committing/upgrading HA path in[COMPLETE] esc` | 24 | RecvOther×20, PunchOK×12 | RecvOther→#11×24 | LEGAL-TRANSIENT |
| 18 | `A:Connected/direct HA path in[-] esc | B:Committing/upgrading HA path in[COMPLETE] -` | 24 | RecvOther×20, PunchOK×12 | RecvOther→#12×24 | LEGAL-TRANSIENT |
| 19 | `A:Failed/relayed -A - in[-] esc | B:Failed/relayed H- - in[-] esc` | 24 | RecvOther×28 | - | LEGAL-TERMINAL RelayedOnly |
| 20 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed -A - in[-] esc` | 24 | RecvOther×30 | - | LEGAL-TERMINAL RelayedOnly |
| 21 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading -- - in[-] -` | 21 | RecvOther×16 | Tick→#67×3, Tick→#66×3, DeliverAny→#147×3, DeliverAny→#142×3 | LEGAL-TRANSIENT |
| 22 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading HA - in[COMPLETE,REJECT] esc` | 20 | PunchOK×20, DeliverAny×4, Tick×2 | RecvOther→#5×20, PunchOK→#23×20 | LEGAL-TRANSIENT |
| 23 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Committing/upgrading HA path in[COMPLETE,REJECT] esc` | 20 | PunchOK×40 | RecvOther→#10×20, RecvOther→#6×20 | LEGAL-TRANSIENT |
| 24 | `A:Punching/upgrading HA - in[-] esc | B:Committing/upgrading HA path in[REJECT] esc` | 20 | RecvOther×20, DeliverAny×4, Tick×2 | PunchOK→#10×20, RecvOther→#7×20 | LEGAL-TRANSIENT |
| 25 | `A:Punching/upgrading HA - in[-] esc | B:Punching/upgrading HA - in[REJECT] esc` | 20 | DeliverAny×8, Tick×4 | PunchOK→#22×20, RecvOther→#8×20, PunchOK→#26×20 | LEGAL-TRANSIENT |
| 26 | `A:Punching/upgrading HA - in[COMPLETE] esc | B:Committing/upgrading HA path in[REJECT] esc` | 20 | PunchOK×20, DeliverAny×4, Tick×2 | RecvOther→#24×20, PunchOK→#23×20, RecvOther→#9×20 | LEGAL-TRANSIENT |
| 27 | `A:Failed/relayed -A - in[-] esc | B:Punching/upgrading H- - in[REJECT] esc` | 18 | PunchFail×8, Tick×3 | RecvOther→#19×12, PunchFail→#78×6 | LEGAL-TRANSIENT |
| 28 | `A:Failed/relayed H- - in[-] esc | B:Committing/upgrading HA path in[REJECT] esc` | 18 | RecvOther×18 | RecvOther→#45×12 | LEGAL-TRANSIENT |
| 29 | `A:Failed/relayed H- - in[-] esc | B:Punching/upgrading HA - in[REJECT] esc` | 18 | DeliverAny×6, Tick×3 | PunchOK→#30×18, RecvOther→#44×12 | LEGAL-TRANSIENT |
| 30 | `A:Failed/relayed H- - in[COMPLETE] esc | B:Committing/upgrading HA path in[REJECT] esc` | 18 | PunchOK×18 | RecvOther→#28×18, RecvOther→#46×12 | LEGAL-TRANSIENT |
| 31 | `A:Punching/upgrading -A - in[-] esc | B:Punching/upgrading H- - in[-] -` | 18 | RecvOther×6, Tick×3 | Tick→#13×6, PunchFail→#65×6 | LEGAL-TRANSIENT |
| 32 | `A:Punching/upgrading H- - in[-] - | B:Punching/upgrading -A - in[-] esc` | 18 | RecvOther×6, Tick×3 | Tick→#14×6, PunchFail→#93×6 | LEGAL-TRANSIENT |
| 33 | `A:Committing/upgrading HA path in[-] - | B:Punching/upgrading HA - in[-] esc` | 12 | RecvOther×12, DeliverAny×6 | PunchOK→#15×12 | LEGAL-TRANSIENT |
| 34 | `A:Committing/upgrading HA path in[-] - | B:Punching/upgrading HA - in[COMPLETE] esc` | 12 | PunchOK×12, DeliverAny×6, RecvOther×4 | RecvOther→#33×12, PunchOK→#39×12 | LEGAL-TRANSIENT |
| 35 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading H- - in[-] esc` | 12 | RecvOther×12 | DeliverAny→#4×12 | LEGAL-TRANSIENT |
| 36 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading H- - in[COMPLETE] esc` | 12 | PunchOK×12, RecvOther×4 | RecvOther→#35×12, DeliverAny→#5×12 | LEGAL-TRANSIENT |
| 37 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading HA - in[-] -` | 12 | RecvOther×12 | PunchOK→#16×12, Tick→#4×6 | LEGAL-TRANSIENT |
| 38 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading HA - in[COMPLETE] -` | 12 | PunchOK×12, RecvOther×4 | RecvOther→#37×12, PunchOK→#40×12, Tick→#5×6 | LEGAL-TRANSIENT |
| 39 | `A:Committing/upgrading HA path in[COMPLETE] - | B:Committing/upgrading HA path in[COMPLETE] esc` | 12 | PunchOK×24, RecvOther×4 | RecvOther→#17×12, RecvOther→#15×12 | LEGAL-TRANSIENT |
| 40 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Committing/upgrading HA path in[COMPLETE] -` | 12 | PunchOK×24, RecvOther×4 | RecvOther→#18×12, RecvOther→#16×12 | LEGAL-TRANSIENT |
| 41 | `A:Committing/upgrading HA path in[REJECT] esc | B:Failed/relayed H- - in[-] esc` | 12 | RecvOther×12 | RecvOther→#48×12 | LEGAL-TRANSIENT |
| 42 | `A:Committing/upgrading HA path in[REJECT] esc | B:Failed/relayed H- - in[COMPLETE] esc` | 12 | PunchOK×12, RecvOther×6 | RecvOther→#49×12, RecvOther→#41×12 | LEGAL-TRANSIENT |
| 43 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed -A - in[REJECT] esc` | 12 | RecvOther×12 | RecvOther→#20×10 | LEGAL-TRANSIENT |
| 44 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed HA - in[-] esc` | 12 | RecvOther×12 | - | LEGAL-TERMINAL RelayedOnly |
| 45 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed HA path in[-] esc` | 12 | RecvOther×24 | - | LEGAL-TERMINAL RelayedOnly |
| 46 | `A:Failed/relayed H- - in[COMPLETE] esc | B:Failed/relayed HA path in[-] esc` | 12 | RecvOther×12 | RecvOther→#45×12 | LEGAL-TRANSIENT |
| 47 | `A:Failed/relayed HA - in[-] esc | B:Failed/relayed H- - in[-] esc` | 12 | RecvOther×18 | - | LEGAL-TERMINAL RelayedOnly |
| 48 | `A:Failed/relayed HA path in[-] esc | B:Failed/relayed H- - in[-] esc` | 12 | RecvOther×24 | - | LEGAL-TERMINAL RelayedOnly |
| 49 | `A:Failed/relayed HA path in[-] esc | B:Failed/relayed H- - in[COMPLETE] esc` | 12 | RecvOther×18 | RecvOther→#48×12 | LEGAL-TRANSIENT |
| 50 | `A:Punching/upgrading H- - in[-] esc | B:Committing/upgrading HA path in[-] esc` | 12 | RecvOther×16 | DeliverAny→#7×12 | LEGAL-TRANSIENT |
| 51 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading HA - in[-] esc` | 12 | DeliverAny×12, RecvOther×4 | PunchOK→#52×12, DeliverAny→#8×12 | LEGAL-TRANSIENT |
| 52 | `A:Punching/upgrading H- - in[COMPLETE] esc | B:Committing/upgrading HA path in[-] esc` | 12 | PunchOK×12, RecvOther×4 | RecvOther→#50×12, DeliverAny→#9×12 | LEGAL-TRANSIENT |
| 53 | `A:Punching/upgrading H- - in[REJECT] esc | B:Failed/relayed -A - in[-] esc` | 12 | PunchFail×6, RecvOther×6, Tick×2 | RecvOther→#20×12, PunchFail→#84×4 | LEGAL-TRANSIENT |
| 54 | `A:Punching/upgrading HA - in[-] - | B:Committing/upgrading HA path in[-] esc` | 12 | RecvOther×16 | PunchOK→#17×12, Tick→#7×6 | LEGAL-TRANSIENT |
| 55 | `A:Punching/upgrading HA - in[-] - | B:Punching/upgrading HA - in[-] esc` | 12 | DeliverAny×6, RecvOther×4 | PunchOK→#34×12, PunchOK→#59×12, Tick→#8×6 | LEGAL-TRANSIENT |
| 56 | `A:Punching/upgrading HA - in[-] esc | B:Committing/upgrading HA path in[-] -` | 12 | RecvOther×16, DeliverAny×6 | PunchOK→#18×12 | LEGAL-TRANSIENT |
| 57 | `A:Punching/upgrading HA - in[-] esc | B:Punching/upgrading H- - in[-] esc` | 12 | DeliverAny×12, RecvOther×4 | PunchOK→#36×12, DeliverAny→#8×12 | LEGAL-TRANSIENT |
| 58 | `A:Punching/upgrading HA - in[-] esc | B:Punching/upgrading HA - in[-] -` | 12 | DeliverAny×6, RecvOther×4 | PunchOK→#38×12, PunchOK→#60×12, Tick→#8×6 | LEGAL-TRANSIENT |
| 59 | `A:Punching/upgrading HA - in[COMPLETE] - | B:Committing/upgrading HA path in[-] esc` | 12 | PunchOK×12, RecvOther×4 | RecvOther→#54×12, PunchOK→#39×12, Tick→#9×6 | LEGAL-TRANSIENT |
| 60 | `A:Punching/upgrading HA - in[COMPLETE] esc | B:Committing/upgrading HA path in[-] -` | 12 | PunchOK×12, DeliverAny×6, RecvOther×4 | RecvOther→#56×12, PunchOK→#40×12 | LEGAL-TRANSIENT |
| 61 | `A:Punching/upgrading HA - in[REJECT] esc | B:Failed/relayed H- - in[-] esc` | 12 | RecvOther×6, DeliverAny×4, Tick×2 | RecvOther→#47×12, PunchOK→#42×12 | LEGAL-TRANSIENT |
| 62 | `A:Failed/relayed -A - in[-] esc | B:Failed/relayed H- - in[REJECT] esc` | 10 | RecvOther×8 | RecvOther→#19×8 | LEGAL-TRANSIENT |
| 63 | `A:Punching/upgrading -A - in[-] esc | B:Punching/upgrading H- - in[REJECT] esc` | 10 | Tick×3 | RecvOther→#13×10, DeliverAny→#126×4, PunchFail→#27×2, PunchFail→#170×2 | LEGAL-TRANSIENT |
| 64 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading -A - in[REJECT] esc` | 10 | Tick×3 | RecvOther→#14×10, DeliverAny→#121×4, PunchFail→#81×2, PunchFail→#94×2 | LEGAL-TRANSIENT |
| 65 | `A:Failed/relayed -A - in[-] esc | B:Punching/upgrading H- - in[REJECT] -` | 9 | PunchFail×8 | RecvOther→#77×6, Tick→#27×3 | LEGAL-TRANSIENT |
| 66 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading -- - in[-] esc` | 9 | RecvOther×7, Tick×3 | DeliverAny→#143×3 | LEGAL-TRANSIENT |
| 67 | `A:Punching/upgrading -- - in[-] esc | B:Punching/upgrading -- - in[-] -` | 9 | RecvOther×5, Tick×3 | DeliverAny→#149×3 | LEGAL-TRANSIENT |
| 68 | `A:Punching/upgrading -A - in[-] - | B:Punching/upgrading H- - in[-] esc` | 9 | Tick×3, DeliverAny×3, RecvOther×3 | DeliverAny→#95×6, Tick→#13×3, PunchFail→#169×3 | LEGAL-TRANSIENT |
| 69 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading -A - in[-] -` | 9 | Tick×3, DeliverAny×3, RecvOther×3 | DeliverAny→#91×6, Tick→#14×3, PunchFail→#136×3 | LEGAL-TRANSIENT |
| 70 | `A:Connected/direct HA path in[-] - | B:Committing/upgrading HA path in[COMPLETE,REJECT] esc` | 8 | RecvOther×4, PunchOK×4 | RecvOther→#17×8 | LEGAL-TRANSIENT |
| 71 | `A:Connected/direct HA path in[-] esc | B:Committing/upgrading HA path in[COMPLETE,REJECT] -` | 8 | PunchOK×4, RecvOther×4 | RecvOther→#18×8 | LEGAL-TRANSIENT |
| 72 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading -- - in[REJECT] -` | 7 | RecvOther×3 | RecvOther→#21×7, Tick→#144×1, Tick→#141×1, DeliverAny→#200×1, DeliverAny→#196×1 | LEGAL-TRANSIENT |
| 73 | `A:AwaitingReady/upgrading -- - in[READY] - | B:Punching/upgrading -- - in[-] -` | 6 | RecvOther×9, ProbeAnswer×2 | RecvOther→#21×6, Tick→#97×2 | LEGAL-TRANSIENT |
| 74 | `A:Committing/upgrading HA path in[-] - | B:Punching/upgrading H- - in[-] esc` | 6 | RecvOther×6 | DeliverAny→#33×6 | LEGAL-TRANSIENT |
| 75 | `A:Committing/upgrading HA path in[-] - | B:Punching/upgrading H- - in[COMPLETE] esc` | 6 | PunchOK×6, RecvOther×2 | RecvOther→#74×6, DeliverAny→#34×6 | LEGAL-TRANSIENT |
| 76 | `A:Committing/upgrading HA path in[REJECT] esc | B:Failed/relayed H- - in[COMPLETE,REJECT] esc` | 6 | PunchOK×6 | RecvOther→#86×6, RecvOther→#42×6 | LEGAL-TRANSIENT |
| 77 | `A:Failed/relayed -A - in[-] esc | B:Failed/relayed H- - in[-] -` | 6 | RecvOther×6 | - | LEGAL-TERMINAL RelayedOnly |
| 78 | `A:Failed/relayed -A - in[REJECT] esc | B:Failed/relayed H- - in[REJECT] esc` | 6 | PunchFail×6 | RecvOther→#62×6, RecvOther→#109×4 | LEGAL-TRANSIENT |
| 79 | `A:Failed/relayed H- - in[-] - | B:Failed/relayed -A - in[-] esc` | 6 | RecvOther×9 | - | LEGAL-TERMINAL RelayedOnly |
| 80 | `A:Failed/relayed H- - in[-] esc | B:Committing/upgrading HA path in[REJECT] -` | 6 | RecvOther×6 | RecvOther→#112×4 | LEGAL-TRANSIENT |
| 81 | `A:Failed/relayed H- - in[-] esc | B:Punching/upgrading -A - in[REJECT] esc` | 6 | PunchFail×8 | DeliverAny→#29×6, RecvOther→#20×4 | LEGAL-TRANSIENT |
| 82 | `A:Failed/relayed H- - in[-] esc | B:Punching/upgrading HA - in[REJECT] -` | 6 | DeliverAny×3 | PunchOK→#83×6, RecvOther→#111×4, Tick→#29×3 | LEGAL-TRANSIENT |
| 83 | `A:Failed/relayed H- - in[COMPLETE] esc | B:Committing/upgrading HA path in[REJECT] -` | 6 | PunchOK×6 | RecvOther→#80×6, RecvOther→#113×4 | LEGAL-TRANSIENT |
| 84 | `A:Failed/relayed H- - in[REJECT] esc | B:Failed/relayed -A - in[REJECT] esc` | 6 | PunchFail×6 | RecvOther→#43×6, RecvOther→#114×4 | LEGAL-TRANSIENT |
| 85 | `A:Failed/relayed HA - in[-] esc | B:Failed/relayed H- - in[REJECT] esc` | 6 | RecvOther×6 | RecvOther→#47×6 | LEGAL-TRANSIENT |
| 86 | `A:Failed/relayed HA path in[-] esc | B:Failed/relayed H- - in[COMPLETE,REJECT] esc` | 6 | RecvOther×6 | RecvOther→#49×6 | LEGAL-TRANSIENT |
| 87 | `A:Proposing/upgrading -- - in[ACCEPT,READY] - | B:Punching/upgrading -- - in[-] -` | 6 | RecvOther×3, ProbeAnswer×2 | RecvOther→#73×6, Tick→#118×2 | LEGAL-TRANSIENT |
| 88 | `A:Punching/upgrading -A - in[-] esc | B:Punching/upgrading H- - in[REJECT] -` | 6 | Tick×1 | RecvOther→#31×6, Tick→#63×2, PunchFail→#65×2 | LEGAL-TRANSIENT |
| 89 | `A:Punching/upgrading H- - in[-] - | B:Punching/upgrading -A - in[REJECT] esc` | 6 | Tick×1 | RecvOther→#32×6, Tick→#64×2, PunchFail→#151×2 | LEGAL-TRANSIENT |
| 90 | `A:Punching/upgrading H- - in[-] esc | B:Committing/upgrading HA path in[-] -` | 6 | RecvOther×8 | DeliverAny→#56×6 | LEGAL-TRANSIENT |
| 91 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading HA - in[-] -` | 6 | DeliverAny×6, RecvOther×2 | PunchOK→#92×6, DeliverAny→#58×6 | LEGAL-TRANSIENT |
| 92 | `A:Punching/upgrading H- - in[COMPLETE] esc | B:Committing/upgrading HA path in[-] -` | 6 | PunchOK×6, RecvOther×2 | RecvOther→#90×6, DeliverAny→#60×6 | LEGAL-TRANSIENT |
| 93 | `A:Punching/upgrading H- - in[REJECT] - | B:Failed/relayed -A - in[-] esc` | 6 | PunchFail×6, RecvOther×3 | RecvOther→#79×6, Tick→#53×2 | LEGAL-TRANSIENT |
| 94 | `A:Punching/upgrading H- - in[REJECT] esc | B:Failed/relayed -A - in[REJECT] esc` | 6 | PunchFail×2, Tick×1 | RecvOther→#43×6, RecvOther→#53×6, PunchFail→#84×2 | LEGAL-TRANSIENT |
| 95 | `A:Punching/upgrading HA - in[-] - | B:Punching/upgrading H- - in[-] esc` | 6 | DeliverAny×6, RecvOther×2 | PunchOK→#75×6, DeliverAny→#55×6 | LEGAL-TRANSIENT |
| 96 | `A:Punching/upgrading HA - in[REJECT] esc | B:Failed/relayed H- - in[REJECT] esc` | 6 | DeliverAny×2, Tick×1 | RecvOther→#85×6, PunchOK→#76×6, RecvOther→#61×6 | LEGAL-TRANSIENT |
| 97 | `A:AwaitingReady/upgrading -- - in[READY] - | B:Punching/upgrading -- - in[-] esc` | 4 | RecvOther×6, Tick×2 | RecvOther→#66×4 | LEGAL-TRANSIENT |
| 98 | `A:Committing/upgrading HA path in[-] - | B:Punching/upgrading HA - in[COMPLETE,REJECT] esc` | 4 | PunchOK×4, DeliverAny×2 | RecvOther→#34×4, PunchOK→#101×4 | LEGAL-TRANSIENT |
| 99 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading H- - in[COMPLETE,REJECT] esc` | 4 | PunchOK×4 | RecvOther→#36×4, DeliverAny→#22×4 | LEGAL-TRANSIENT |
| 100 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading HA - in[COMPLETE,REJECT] -` | 4 | PunchOK×4 | RecvOther→#38×4, PunchOK→#102×4, Tick→#22×2 | LEGAL-TRANSIENT |
| 101 | `A:Committing/upgrading HA path in[COMPLETE] - | B:Committing/upgrading HA path in[COMPLETE,REJECT] esc` | 4 | PunchOK×8 | RecvOther→#70×4, RecvOther→#39×4 | LEGAL-TRANSIENT |
| 102 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Committing/upgrading HA path in[COMPLETE,REJECT] -` | 4 | PunchOK×8 | RecvOther→#71×4, RecvOther→#40×4 | LEGAL-TRANSIENT |
| 103 | `A:Committing/upgrading HA path in[REJECT] - | B:Failed/relayed H- - in[-] esc` | 4 | RecvOther×4 | RecvOther→#116×4 | LEGAL-TRANSIENT |
| 104 | `A:Committing/upgrading HA path in[REJECT] - | B:Failed/relayed H- - in[COMPLETE] esc` | 4 | PunchOK×4, RecvOther×2 | RecvOther→#117×4, RecvOther→#103×4 | LEGAL-TRANSIENT |
| 105 | `A:Failed/relayed -- - in[-] - | B:Punching/upgrading -- - in[-] esc` | 4 | RecvOther×4, Tick×1 | PunchFail→#159×2 | LEGAL-TRANSIENT |
| 106 | `A:Failed/relayed -- - in[ACCEPT,READY] - | B:Punching/upgrading -- - in[-] esc` | 4 | Tick×1 | RecvOther→#107×4, PunchFail→#157×2 | LEGAL-TRANSIENT |
| 107 | `A:Failed/relayed -- - in[READY] - | B:Punching/upgrading -- - in[-] esc` | 4 | RecvOther×4, Tick×1 | RecvOther→#105×4, PunchFail→#158×2 | LEGAL-TRANSIENT |
| 108 | `A:Failed/relayed -A - in[-] - | B:Failed/relayed H- - in[-] esc` | 4 | RecvOther×4 | - | LEGAL-TERMINAL RelayedOnly |
| 109 | `A:Failed/relayed -A - in[REJECT] esc | B:Failed/relayed H- - in[-] esc` | 4 | RecvOther×4 | RecvOther→#19×4 | LEGAL-TRANSIENT |
| 110 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed -A - in[-] -` | 4 | RecvOther×2 | - | LEGAL-TERMINAL RelayedOnly |
| 111 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed HA - in[-] -` | 4 | RecvOther×4 | - | LEGAL-TERMINAL RelayedOnly |
| 112 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed HA path in[-] -` | 4 | RecvOther×8 | - | LEGAL-TERMINAL RelayedOnly |
| 113 | `A:Failed/relayed H- - in[COMPLETE] esc | B:Failed/relayed HA path in[-] -` | 4 | RecvOther×4 | RecvOther→#112×4 | LEGAL-TRANSIENT |
| 114 | `A:Failed/relayed H- - in[REJECT] esc | B:Failed/relayed -A - in[-] esc` | 4 | RecvOther×4 | RecvOther→#20×4 | LEGAL-TRANSIENT |
| 115 | `A:Failed/relayed HA - in[-] - | B:Failed/relayed H- - in[-] esc` | 4 | RecvOther×6 | - | LEGAL-TERMINAL RelayedOnly |
| 116 | `A:Failed/relayed HA path in[-] - | B:Failed/relayed H- - in[-] esc` | 4 | RecvOther×8 | - | LEGAL-TERMINAL RelayedOnly |
| 117 | `A:Failed/relayed HA path in[-] - | B:Failed/relayed H- - in[COMPLETE] esc` | 4 | RecvOther×6 | RecvOther→#116×4 | LEGAL-TRANSIENT |
| 118 | `A:Proposing/upgrading -- - in[ACCEPT,READY] - | B:Punching/upgrading -- - in[-] esc` | 4 | Tick×2, RecvOther×2 | RecvOther→#97×4 | LEGAL-TRANSIENT |
| 119 | `A:Punching/upgrading -A - in[REJECT] esc | B:Failed/relayed H- - in[-] esc` | 4 | PunchFail×6, RecvOther×2 | RecvOther→#19×4, DeliverAny→#61×4 | LEGAL-TRANSIENT |
| 120 | `A:Punching/upgrading H- - in[-] esc | B:Committing/upgrading HA path in[REJECT] esc` | 4 | RecvOther×4 | RecvOther→#50×4, DeliverAny→#24×4 | LEGAL-TRANSIENT |
| 121 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading HA - in[REJECT] esc` | 4 | DeliverAny×4 | RecvOther→#51×4, PunchOK→#122×4, DeliverAny→#25×4 | LEGAL-TRANSIENT |
| 122 | `A:Punching/upgrading H- - in[COMPLETE] esc | B:Committing/upgrading HA path in[REJECT] esc` | 4 | PunchOK×4 | RecvOther→#120×4, RecvOther→#52×4, DeliverAny→#26×4 | LEGAL-TRANSIENT |
| 123 | `A:Punching/upgrading HA - in[-] - | B:Committing/upgrading HA path in[REJECT] esc` | 4 | RecvOther×4 | PunchOK→#70×4, RecvOther→#54×4, Tick→#24×2 | LEGAL-TRANSIENT |
| 124 | `A:Punching/upgrading HA - in[-] - | B:Punching/upgrading HA - in[REJECT] esc` | 4 | DeliverAny×2 | PunchOK→#98×4, RecvOther→#55×4, PunchOK→#128×4, Tick→#25×2 | LEGAL-TRANSIENT |
| 125 | `A:Punching/upgrading HA - in[-] esc | B:Committing/upgrading HA path in[REJECT] -` | 4 | RecvOther×4, DeliverAny×2 | PunchOK→#71×4, RecvOther→#56×4 | LEGAL-TRANSIENT |
| 126 | `A:Punching/upgrading HA - in[-] esc | B:Punching/upgrading H- - in[REJECT] esc` | 4 | DeliverAny×4 | PunchOK→#99×4, RecvOther→#57×4, DeliverAny→#25×4 | LEGAL-TRANSIENT |
| 127 | `A:Punching/upgrading HA - in[-] esc | B:Punching/upgrading HA - in[REJECT] -` | 4 | DeliverAny×2 | PunchOK→#100×4, RecvOther→#58×4, PunchOK→#129×4, Tick→#25×2 | LEGAL-TRANSIENT |
| 128 | `A:Punching/upgrading HA - in[COMPLETE] - | B:Committing/upgrading HA path in[REJECT] esc` | 4 | PunchOK×4 | RecvOther→#123×4, PunchOK→#101×4, RecvOther→#59×4, Tick→#26×2 | LEGAL-TRANSIENT |
| 129 | `A:Punching/upgrading HA - in[COMPLETE] esc | B:Committing/upgrading HA path in[REJECT] -` | 4 | PunchOK×4, DeliverAny×2 | RecvOther→#125×4, PunchOK→#102×4, RecvOther→#60×4 | LEGAL-TRANSIENT |
| 130 | `A:Punching/upgrading HA - in[REJECT] - | B:Failed/relayed H- - in[-] esc` | 4 | DeliverAny×2, RecvOther×2 | RecvOther→#115×4, PunchOK→#104×4, Tick→#61×2 | LEGAL-TRANSIENT |
| 131 | `A:AwaitingReady/upgrading -- - in[READY] - | B:Punching/upgrading -- - in[REJECT] -` | 3 | RecvOther×3, ProbeAnswer×1 | RecvOther→#72×3, RecvOther→#73×3, Tick→#153×1 | LEGAL-TRANSIENT |
| 132 | `A:Failed/relayed -- - in[-] - | B:Punching/upgrading -- - in[-] -` | 3 | RecvOther×3 | Tick→#105×1 | LEGAL-TRANSIENT |
| 133 | `A:Failed/relayed -- - in[ACCEPT,READY] - | B:Punching/upgrading -- - in[-] -` | 3 | ProbeAnswer×1 | RecvOther→#134×3, Tick→#106×1 | LEGAL-TRANSIENT |
| 134 | `A:Failed/relayed -- - in[READY] - | B:Punching/upgrading -- - in[-] -` | 3 | RecvOther×3, ProbeAnswer×1 | RecvOther→#132×3, Tick→#107×1 | LEGAL-TRANSIENT |
| 135 | `A:Failed/relayed H- - in[-] - | B:Failed/relayed -A - in[REJECT] esc` | 3 | RecvOther×3 | RecvOther→#79×3 | LEGAL-TRANSIENT |
| 136 | `A:Failed/relayed H- - in[-] esc | B:Punching/upgrading -A - in[REJECT] -` | 3 | PunchFail×4 | DeliverAny→#82×3, RecvOther→#110×2 | LEGAL-TRANSIENT |
| 137 | `A:Proposing/upgrading -- - in[ACCEPT,READY,REQUEST] - | B:Punching/upgrading -- - in[-] -` | 3 | ProbeAnswer×1 | RecvRequest→#138×3, Tick→#164×1 | LEGAL-TRANSIENT |
| 138 | `A:Proposing/upgrading -- - in[ACCEPT,READY] - | B:Punching/upgrading -- - in[REJECT] -` | 3 | RecvRequest×3, ProbeAnswer×1 | RecvOther→#131×3, RecvOther→#87×3, Tick→#165×1 | LEGAL-TRANSIENT |
| 139 | `A:Punching/upgrading -- - in[-] - | B:AwaitingReady/upgrading -- - in[READY] -` | 3 | RecvOther×3, ProbeAnswer×1 | RecvOther→#21×3, Tick→#167×1 | LEGAL-TRANSIENT |
| 140 | `A:Punching/upgrading -- - in[-] - | B:Proposing/upgrading -- - in[ACCEPT,READY] -` | 3 | ProbeAnswer×1 | RecvOther→#139×3, Tick→#168×1 | LEGAL-TRANSIENT |
| 141 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading -- - in[REJECT] esc` | 3 | RecvOther×2, Tick×1 | RecvOther→#66×3, DeliverAny→#197×1 | LEGAL-TRANSIENT |
| 142 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading H- - in[-] -` | 3 | DeliverAny×3, RecvOther×1 | DeliverAny→#145×3 | LEGAL-TRANSIENT |
| 143 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading H- - in[-] esc` | 3 | DeliverAny×3, RecvOther×1 | DeliverAny→#68×3 | LEGAL-TRANSIENT |
| 144 | `A:Punching/upgrading -- - in[-] esc | B:Punching/upgrading -- - in[REJECT] -` | 3 | Tick×1 | RecvOther→#67×3, DeliverAny→#202×1 | LEGAL-TRANSIENT |
| 145 | `A:Punching/upgrading -A - in[-] - | B:Punching/upgrading H- - in[-] -` | 3 | DeliverAny×3, RecvOther×1 | Tick→#31×3, Tick→#68×3 | LEGAL-TRANSIENT |
| 146 | `A:Punching/upgrading -A - in[-] - | B:Punching/upgrading H- - in[REJECT] esc` | 3 | Tick×1, DeliverAny×1 | RecvOther→#68×3, DeliverAny→#174×2, Tick→#63×1, PunchFail→#199×1 | LEGAL-TRANSIENT |
| 147 | `A:Punching/upgrading H- - in[-] - | B:Punching/upgrading -- - in[-] -` | 3 | DeliverAny×3, RecvOther×1 | DeliverAny→#148×3 | LEGAL-TRANSIENT |
| 148 | `A:Punching/upgrading H- - in[-] - | B:Punching/upgrading -A - in[-] -` | 3 | DeliverAny×3, RecvOther×1 | Tick→#69×3, Tick→#32×3 | LEGAL-TRANSIENT |
| 149 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading -- - in[-] -` | 3 | DeliverAny×3, RecvOther×1 | DeliverAny→#69×3 | LEGAL-TRANSIENT |
| 150 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading -A - in[REJECT] -` | 3 | Tick×1, DeliverAny×1 | RecvOther→#69×3, DeliverAny→#172×2, Tick→#64×1, PunchFail→#136×1 | LEGAL-TRANSIENT |
| 151 | `A:Punching/upgrading H- - in[REJECT] - | B:Failed/relayed -A - in[REJECT] esc` | 3 | PunchFail×2 | RecvOther→#135×3, RecvOther→#93×3, Tick→#94×1 | LEGAL-TRANSIENT |
| 152 | `A:AwaitingReady/upgrading -- - in[-] - | B:Discovering/upgrading -- - in[-] -` | 2 | RecvOther×3 | ProbeAnswer→#73×2 | LEGAL-TRANSIENT |
| 153 | `A:AwaitingReady/upgrading -- - in[READY] - | B:Punching/upgrading -- - in[REJECT] esc` | 2 | RecvOther×2, Tick×1 | RecvOther→#141×2, RecvOther→#97×2 | LEGAL-TRANSIENT |
| 154 | `A:Committing/upgrading HA path in[-] - | B:Punching/upgrading H- - in[COMPLETE,REJECT] esc` | 2 | PunchOK×2 | RecvOther→#75×2, DeliverAny→#98×2 | LEGAL-TRANSIENT |
| 155 | `A:Committing/upgrading HA path in[REJECT] - | B:Failed/relayed H- - in[COMPLETE,REJECT] esc` | 2 | PunchOK×2 | RecvOther→#162×2, RecvOther→#104×2 | LEGAL-TRANSIENT |
| 156 | `A:Failed/relayed -- - in[-] - | B:Failed/relayed -- - in[-] esc` | 2 | RecvOther×2 | - | LEGAL-TERMINAL RelayedOnly |
| 157 | `A:Failed/relayed -- - in[ACCEPT,READY,REJECT] - | B:Failed/relayed -- - in[-] esc` | 2 | PunchFail×2 | RecvOther→#158×2 | LEGAL-TRANSIENT |
| 158 | `A:Failed/relayed -- - in[READY,REJECT] - | B:Failed/relayed -- - in[-] esc` | 2 | PunchFail×2, RecvOther×2 | RecvOther→#159×2 | LEGAL-TRANSIENT |
| 159 | `A:Failed/relayed -- - in[REJECT] - | B:Failed/relayed -- - in[-] esc` | 2 | PunchFail×2, RecvOther×2 | RecvOther→#156×2 | LEGAL-TRANSIENT |
| 160 | `A:Failed/relayed -A - in[-] - | B:Failed/relayed H- - in[REJECT] esc` | 2 | RecvOther×1 | RecvOther→#108×2 | LEGAL-TRANSIENT |
| 161 | `A:Failed/relayed HA - in[-] - | B:Failed/relayed H- - in[REJECT] esc` | 2 | RecvOther×2 | RecvOther→#115×2 | LEGAL-TRANSIENT |
| 162 | `A:Failed/relayed HA path in[-] - | B:Failed/relayed H- - in[COMPLETE,REJECT] esc` | 2 | RecvOther×2 | RecvOther→#117×2 | LEGAL-TRANSIENT |
| 163 | `A:Proposing/upgrading -- - in[-] - | B:Proposing/upgrading -- - in[REJECT,REQUEST] -` | 2 | RecvRequest×1, ProbeAnswer×1 | RecvRequest→#194×1, RecvOther→#191×1 | LEGAL-TRANSIENT |
| 164 | `A:Proposing/upgrading -- - in[ACCEPT,READY,REQUEST] - | B:Punching/upgrading -- - in[-] esc` | 2 | Tick×1 | RecvRequest→#165×2 | LEGAL-TRANSIENT |
| 165 | `A:Proposing/upgrading -- - in[ACCEPT,READY] - | B:Punching/upgrading -- - in[REJECT] esc` | 2 | RecvRequest×2, Tick×1 | RecvOther→#153×2, RecvOther→#118×2 | LEGAL-TRANSIENT |
| 166 | `A:Proposing/upgrading -- - in[ACCEPT] - | B:Discovering/upgrading -- - in[-] -` | 2 | RecvRequest×3, RecvOther×1 | RecvOther→#152×2, ProbeAnswer→#87×2 | LEGAL-TRANSIENT |
| 167 | `A:Punching/upgrading -- - in[-] esc | B:AwaitingReady/upgrading -- - in[READY] -` | 2 | RecvOther×2, Tick×1 | RecvOther→#67×2 | LEGAL-TRANSIENT |
| 168 | `A:Punching/upgrading -- - in[-] esc | B:Proposing/upgrading -- - in[ACCEPT,READY] -` | 2 | Tick×1 | RecvOther→#167×2 | LEGAL-TRANSIENT |
| 169 | `A:Punching/upgrading -A - in[REJECT] - | B:Failed/relayed H- - in[-] esc` | 2 | PunchFail×3, RecvOther×1 | RecvOther→#108×2, DeliverAny→#130×2 | LEGAL-TRANSIENT |
| 170 | `A:Punching/upgrading -A - in[REJECT] esc | B:Failed/relayed H- - in[REJECT] esc` | 2 | PunchFail×2 | RecvOther→#62×2, RecvOther→#119×2, DeliverAny→#96×2 | LEGAL-TRANSIENT |
| 171 | `A:Punching/upgrading H- - in[-] esc | B:Committing/upgrading HA path in[REJECT] -` | 2 | RecvOther×2 | RecvOther→#90×2, DeliverAny→#125×2 | LEGAL-TRANSIENT |
| 172 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading HA - in[REJECT] -` | 2 | DeliverAny×2 | RecvOther→#91×2, PunchOK→#173×2, DeliverAny→#127×2 | LEGAL-TRANSIENT |
| 173 | `A:Punching/upgrading H- - in[COMPLETE] esc | B:Committing/upgrading HA path in[REJECT] -` | 2 | PunchOK×2 | RecvOther→#171×2, RecvOther→#92×2, DeliverAny→#129×2 | LEGAL-TRANSIENT |
| 174 | `A:Punching/upgrading HA - in[-] - | B:Punching/upgrading H- - in[REJECT] esc` | 2 | DeliverAny×2 | PunchOK→#154×2, RecvOther→#95×2, DeliverAny→#124×2 | LEGAL-TRANSIENT |
| 175 | `A:Punching/upgrading HA - in[REJECT] - | B:Failed/relayed H- - in[REJECT] esc` | 2 | DeliverAny×1 | RecvOther→#161×2, PunchOK→#155×2, RecvOther→#130×2, Tick→#96×1 | LEGAL-TRANSIENT |
| 176 | `A:AwaitingReady/upgrading -- - in[-] - | B:Discovering/upgrading -- - in[REJECT] -` | 1 | RecvOther×1 | ProbeAnswer→#131×1, RecvOther→#152×1 | LEGAL-TRANSIENT |
| 177 | `A:Discovering/upgrading -- - in[-] - | B:AwaitingReady/upgrading -- - in[-] -` | 1 | RecvOther×1 | ProbeAnswer→#139×1 | LEGAL-TRANSIENT |
| 178 | `A:Discovering/upgrading -- - in[-] - | B:Discovering/upgrading -- - in[-] -` | 1 | Propose×3 | ProbeAnswer→#190×1, ProbeAnswer→#183×1 | LEGAL-TRANSIENT |
| 179 | `A:Discovering/upgrading -- - in[-] - | B:Failed/relayed -- - in[-] -` | 1 | RecvOther×1 | ProbeAnswer→#191×1 | LEGAL-TRANSIENT |
| 180 | `A:Discovering/upgrading -- - in[-] - | B:Idle/relayed -- - in[-] -` | 1 | Propose×2 | ProbeAnswer→#192×1, Propose→#178×1 | LEGAL-TRANSIENT |
| 181 | `A:Discovering/upgrading -- - in[-] - | B:Proposing/upgrading -- - in[ACCEPT] -` | 1 | RecvRequest×1 | ProbeAnswer→#140×1, RecvOther→#177×1 | LEGAL-TRANSIENT |
| 182 | `A:Discovering/upgrading -- - in[-] - | B:Proposing/upgrading -- - in[REJECT] -` | 1 | RecvRequest×1 | ProbeAnswer→#163×1, RecvOther→#179×1 | LEGAL-TRANSIENT |
| 183 | `A:Discovering/upgrading -- - in[REQUEST] - | B:Proposing/upgrading -- - in[-] -` | 1 | Propose×2, ProbeAnswer×1 | ProbeAnswer→#195×1, RecvRequest→#182×1 | LEGAL-TRANSIENT |
| 184 | `A:Failed/relayed -- - in[-] - | B:Discovering/upgrading -- - in[-] -` | 1 | RecvOther×1 | ProbeAnswer→#134×1 | LEGAL-TRANSIENT |
| 185 | `A:Failed/relayed -- - in[-] - | B:Failed/relayed -- - in[REQUEST] -` | 1 | Timeout×1 | RecvRequest→#186×1 | LEGAL-TRANSIENT |
| 186 | `A:Failed/relayed -- - in[ACCEPT] - | B:Discovering/upgrading -- - in[-] -` | 1 | RecvRequest×1 | RecvOther→#184×1, ProbeAnswer→#133×1 | LEGAL-TRANSIENT |
| 187 | `A:Idle/relayed -- - in[-] - | B:Discovering/upgrading -- - in[-] -` | 1 | Propose×1 | Propose→#178×2, ProbeAnswer→#189×1 | LEGAL-TRANSIENT |
| 188 | `A:Idle/relayed -- - in[-] - | B:Idle/relayed -- - in[-] -` | 1 | - | Propose→#180×2, Propose→#187×1 | LEGAL-TRANSIENT |
| 189 | `A:Idle/relayed -- - in[REQUEST] - | B:Proposing/upgrading -- - in[-] -` | 1 | ProbeAnswer×1 | Propose→#183×2, RecvRequest→#181×1 | LEGAL-TRANSIENT |
| 190 | `A:Proposing/upgrading -- - in[-] - | B:Discovering/upgrading -- - in[REQUEST] -` | 1 | Propose×1, ProbeAnswer×1 | ProbeAnswer→#195×1, RecvRequest→#166×1 | LEGAL-TRANSIENT |
| 191 | `A:Proposing/upgrading -- - in[-] - | B:Failed/relayed -- - in[REQUEST] -` | 1 | RecvOther×1, ProbeAnswer×1 | Timeout→#185×1, RecvRequest→#166×1 | LEGAL-TRANSIENT |
| 192 | `A:Proposing/upgrading -- - in[-] - | B:Idle/relayed -- - in[REQUEST] -` | 1 | ProbeAnswer×1 | Propose→#190×1, RecvRequest→#166×1 | LEGAL-TRANSIENT |
| 193 | `A:Proposing/upgrading -- - in[ACCEPT,REQUEST] - | B:Discovering/upgrading -- - in[-] -` | 1 | RecvRequest×1 | RecvRequest→#194×1, ProbeAnswer→#137×1 | LEGAL-TRANSIENT |
| 194 | `A:Proposing/upgrading -- - in[ACCEPT] - | B:Discovering/upgrading -- - in[REJECT] -` | 1 | RecvRequest×2 | RecvOther→#176×1, ProbeAnswer→#138×1, RecvOther→#166×1 | LEGAL-TRANSIENT |
| 195 | `A:Proposing/upgrading -- - in[REQUEST] - | B:Proposing/upgrading -- - in[REQUEST] -` | 1 | ProbeAnswer×2 | RecvRequest→#163×1, RecvRequest→#193×1 | LEGAL-TRANSIENT |
| 196 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading H- - in[REJECT] -` | 1 | DeliverAny×1 | RecvOther→#142×1, DeliverAny→#198×1 | LEGAL-TRANSIENT |
| 197 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading H- - in[REJECT] esc` | 1 | DeliverAny×1 | RecvOther→#143×1, DeliverAny→#146×1 | LEGAL-TRANSIENT |
| 198 | `A:Punching/upgrading -A - in[-] - | B:Punching/upgrading H- - in[REJECT] -` | 1 | DeliverAny×1 | Tick→#88×1, RecvOther→#145×1, Tick→#146×1 | LEGAL-TRANSIENT |
| 199 | `A:Punching/upgrading -A - in[REJECT] - | B:Failed/relayed H- - in[REJECT] esc` | 1 | PunchFail×1 | RecvOther→#160×1, RecvOther→#169×1, DeliverAny→#175×1 | LEGAL-TRANSIENT |
| 200 | `A:Punching/upgrading H- - in[-] - | B:Punching/upgrading -- - in[REJECT] -` | 1 | DeliverAny×1 | RecvOther→#147×1, DeliverAny→#201×1 | LEGAL-TRANSIENT |
| 201 | `A:Punching/upgrading H- - in[-] - | B:Punching/upgrading -A - in[REJECT] -` | 1 | DeliverAny×1 | Tick→#150×1, RecvOther→#148×1, Tick→#89×1 | LEGAL-TRANSIENT |
| 202 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading -- - in[REJECT] -` | 1 | DeliverAny×1 | RecvOther→#149×1, DeliverAny→#150×1 | LEGAL-TRANSIENT |

Classes with no path to a terminal state, or a rejected terminal: 0.
