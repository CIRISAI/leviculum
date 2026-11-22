### State classes: keepalive-lost

1002 states, 1920 transitions, 122 classes. Class: per side `phase/linkPath heard-acked path in[signals in flight] flags`.

| # | class (A \| B) | states | in (by action) | out (action -> class) | verdict |
|---|---|---|---|---|---|
| 1 | `A:Connected/direct HA path in[-] esc | B:Connected/direct HA path in[-] esc` | 60 | RecvOther×80 | FallBack→#3×60, FallBack→#2×60 | LEGAL-TRANSIENT |
| 2 | `A:Connected/direct HA path in[-] esc | B:Connected/relayed HA path in[-] esc` | 60 | FallBack×60, RecvOther×40 | FallBack→#4×60 | LEGAL-TRANSIENT |
| 3 | `A:Connected/relayed HA path in[-] esc | B:Connected/direct HA path in[-] esc` | 60 | FallBack×60, RecvOther×40 | FallBack→#4×60 | LEGAL-TRANSIENT |
| 4 | `A:Connected/relayed HA path in[-] esc | B:Connected/relayed HA path in[-] esc` | 60 | FallBack×120 | - | LEGAL-TERMINAL RelayedOnly |
| 5 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Connected/direct HA path in[-] esc` | 40 | PunchOK×20, RecvOther×20 | RecvOther→#1×40, FallBack→#6×40 | LEGAL-TRANSIENT |
| 6 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Connected/relayed HA path in[-] esc` | 40 | FallBack×40 | RecvOther→#2×40 | LEGAL-TRANSIENT |
| 7 | `A:Connected/direct HA path in[-] esc | B:Committing/upgrading HA path in[COMPLETE] esc` | 40 | RecvOther×20, PunchOK×20 | FallBack→#8×40, RecvOther→#1×40 | LEGAL-TRANSIENT |
| 8 | `A:Connected/relayed HA path in[-] esc | B:Committing/upgrading HA path in[COMPLETE] esc` | 40 | FallBack×40 | RecvOther→#3×40 | LEGAL-TRANSIENT |
| 9 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading HA - in[-] esc` | 20 | RecvOther×20, DeliverAny×4, Tick×2 | PunchOK→#5×20 | LEGAL-TRANSIENT |
| 10 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading HA - in[COMPLETE] esc` | 20 | PunchOK×20, DeliverAny×4, Tick×2 | RecvOther→#9×20, PunchOK→#11×20 | LEGAL-TRANSIENT |
| 11 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Committing/upgrading HA path in[COMPLETE] esc` | 20 | PunchOK×40 | RecvOther→#7×20, RecvOther→#5×20 | LEGAL-TRANSIENT |
| 12 | `A:Punching/upgrading HA - in[-] esc | B:Committing/upgrading HA path in[-] esc` | 20 | RecvOther×20, DeliverAny×4, Tick×2 | PunchOK→#7×20 | LEGAL-TRANSIENT |
| 13 | `A:Punching/upgrading HA - in[-] esc | B:Punching/upgrading HA - in[-] esc` | 20 | DeliverAny×8, Tick×4 | PunchOK→#10×20, PunchOK→#14×20 | LEGAL-TRANSIENT |
| 14 | `A:Punching/upgrading HA - in[COMPLETE] esc | B:Committing/upgrading HA path in[-] esc` | 20 | PunchOK×20, DeliverAny×4, Tick×2 | RecvOther→#12×20, PunchOK→#11×20 | LEGAL-TRANSIENT |
| 15 | `A:Connected/direct HA path in[-] - | B:Connected/direct HA path in[-] esc` | 12 | RecvOther×16 | FallBack→#19×12, FallBack→#16×12 | LEGAL-TRANSIENT |
| 16 | `A:Connected/direct HA path in[-] - | B:Connected/relayed HA path in[-] esc` | 12 | FallBack×12, RecvOther×8 | FallBack→#20×12 | LEGAL-TRANSIENT |
| 17 | `A:Connected/direct HA path in[-] esc | B:Connected/direct HA path in[-] -` | 12 | RecvOther×16 | FallBack→#21×12, FallBack→#18×12 | LEGAL-TRANSIENT |
| 18 | `A:Connected/direct HA path in[-] esc | B:Connected/relayed HA path in[-] -` | 12 | FallBack×12, RecvOther×8 | FallBack→#22×12 | LEGAL-TRANSIENT |
| 19 | `A:Connected/relayed HA path in[-] - | B:Connected/direct HA path in[-] esc` | 12 | FallBack×12, RecvOther×8 | FallBack→#20×12 | LEGAL-TRANSIENT |
| 20 | `A:Connected/relayed HA path in[-] - | B:Connected/relayed HA path in[-] esc` | 12 | FallBack×24 | - | LEGAL-TERMINAL RelayedOnly |
| 21 | `A:Connected/relayed HA path in[-] esc | B:Connected/direct HA path in[-] -` | 12 | FallBack×12, RecvOther×8 | FallBack→#22×12 | LEGAL-TRANSIENT |
| 22 | `A:Connected/relayed HA path in[-] esc | B:Connected/relayed HA path in[-] -` | 12 | FallBack×24 | - | LEGAL-TERMINAL RelayedOnly |
| 23 | `A:Failed/relayed -A - in[-] esc | B:Failed/relayed H- - in[-] esc` | 12 | RecvOther×12 | - | LEGAL-TERMINAL RelayedOnly |
| 24 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed -A - in[-] esc` | 12 | RecvOther×12 | - | LEGAL-TERMINAL RelayedOnly |
| 25 | `A:Punching/upgrading -A - in[-] esc | B:Punching/upgrading H- - in[-] esc` | 10 | Tick×3 | DeliverAny→#66×4, PunchFail→#38×2, PunchFail→#104×2 | LEGAL-TRANSIENT |
| 26 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading -A - in[-] esc` | 10 | Tick×3 | DeliverAny→#61×4, PunchFail→#94×2, PunchFail→#50×2 | LEGAL-TRANSIENT |
| 27 | `A:Committing/upgrading HA path in[COMPLETE] - | B:Connected/direct HA path in[-] esc` | 8 | PunchOK×4, RecvOther×4 | RecvOther→#15×8, FallBack→#28×8 | LEGAL-TRANSIENT |
| 28 | `A:Committing/upgrading HA path in[COMPLETE] - | B:Connected/relayed HA path in[-] esc` | 8 | FallBack×8 | RecvOther→#16×8 | LEGAL-TRANSIENT |
| 29 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Connected/direct HA path in[-] -` | 8 | RecvOther×4, PunchOK×4 | RecvOther→#17×8, FallBack→#30×8 | LEGAL-TRANSIENT |
| 30 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Connected/relayed HA path in[-] -` | 8 | FallBack×8 | RecvOther→#18×8 | LEGAL-TRANSIENT |
| 31 | `A:Connected/direct HA path in[-] - | B:Committing/upgrading HA path in[COMPLETE] esc` | 8 | RecvOther×4, PunchOK×4 | FallBack→#33×8, RecvOther→#15×8 | LEGAL-TRANSIENT |
| 32 | `A:Connected/direct HA path in[-] esc | B:Committing/upgrading HA path in[COMPLETE] -` | 8 | PunchOK×4, RecvOther×4 | FallBack→#34×8, RecvOther→#17×8 | LEGAL-TRANSIENT |
| 33 | `A:Connected/relayed HA path in[-] - | B:Committing/upgrading HA path in[COMPLETE] esc` | 8 | FallBack×8 | RecvOther→#19×8 | LEGAL-TRANSIENT |
| 34 | `A:Connected/relayed HA path in[-] esc | B:Committing/upgrading HA path in[COMPLETE] -` | 8 | FallBack×8 | RecvOther→#21×8 | LEGAL-TRANSIENT |
| 35 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading -- - in[-] -` | 7 | RecvOther×3 | Tick→#76×1, Tick→#75×1, DeliverAny→#120×1, DeliverAny→#116×1 | LEGAL-TRANSIENT |
| 36 | `A:Committing/upgrading HA path in[REJECT] esc | B:Failed/relayed H- - in[-] esc` | 6 | RecvOther×6 | RecvOther→#46×6 | LEGAL-TRANSIENT |
| 37 | `A:Committing/upgrading HA path in[REJECT] esc | B:Failed/relayed H- - in[COMPLETE] esc` | 6 | PunchOK×6 | RecvOther→#47×6, RecvOther→#36×6 | LEGAL-TRANSIENT |
| 38 | `A:Failed/relayed -A - in[-] esc | B:Punching/upgrading H- - in[REJECT] esc` | 6 | PunchFail×2, Tick×1 | RecvOther→#23×6, PunchFail→#88×2 | LEGAL-TRANSIENT |
| 39 | `A:Failed/relayed H- - in[-] esc | B:Committing/upgrading HA path in[REJECT] esc` | 6 | RecvOther×6 | RecvOther→#41×6 | LEGAL-TRANSIENT |
| 40 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed HA - in[-] esc` | 6 | RecvOther×6 | - | LEGAL-TERMINAL RelayedOnly |
| 41 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed HA path in[-] esc` | 6 | RecvOther×12 | - | LEGAL-TERMINAL RelayedOnly |
| 42 | `A:Failed/relayed H- - in[-] esc | B:Punching/upgrading HA - in[REJECT] esc` | 6 | DeliverAny×2, Tick×1 | RecvOther→#40×6, PunchOK→#43×6 | LEGAL-TRANSIENT |
| 43 | `A:Failed/relayed H- - in[COMPLETE] esc | B:Committing/upgrading HA path in[REJECT] esc` | 6 | PunchOK×6 | RecvOther→#39×6, RecvOther→#44×6 | LEGAL-TRANSIENT |
| 44 | `A:Failed/relayed H- - in[COMPLETE] esc | B:Failed/relayed HA path in[-] esc` | 6 | RecvOther×6 | RecvOther→#41×6 | LEGAL-TRANSIENT |
| 45 | `A:Failed/relayed HA - in[-] esc | B:Failed/relayed H- - in[-] esc` | 6 | RecvOther×6 | - | LEGAL-TERMINAL RelayedOnly |
| 46 | `A:Failed/relayed HA path in[-] esc | B:Failed/relayed H- - in[-] esc` | 6 | RecvOther×12 | - | LEGAL-TERMINAL RelayedOnly |
| 47 | `A:Failed/relayed HA path in[-] esc | B:Failed/relayed H- - in[COMPLETE] esc` | 6 | RecvOther×6 | RecvOther→#46×6 | LEGAL-TRANSIENT |
| 48 | `A:Punching/upgrading -A - in[-] esc | B:Punching/upgrading H- - in[-] -` | 6 | Tick×1 | Tick→#25×2, PunchFail→#72×2 | LEGAL-TRANSIENT |
| 49 | `A:Punching/upgrading H- - in[-] - | B:Punching/upgrading -A - in[-] esc` | 6 | Tick×1 | Tick→#26×2, PunchFail→#79×2 | LEGAL-TRANSIENT |
| 50 | `A:Punching/upgrading H- - in[REJECT] esc | B:Failed/relayed -A - in[-] esc` | 6 | PunchFail×2, Tick×1 | RecvOther→#24×6, PunchFail→#99×2 | LEGAL-TRANSIENT |
| 51 | `A:Punching/upgrading HA - in[REJECT] esc | B:Failed/relayed H- - in[-] esc` | 6 | DeliverAny×2, Tick×1 | RecvOther→#45×6, PunchOK→#37×6 | LEGAL-TRANSIENT |
| 52 | `A:Committing/upgrading HA path in[-] - | B:Punching/upgrading HA - in[-] esc` | 4 | RecvOther×4, DeliverAny×2 | PunchOK→#27×4 | LEGAL-TRANSIENT |
| 53 | `A:Committing/upgrading HA path in[-] - | B:Punching/upgrading HA - in[COMPLETE] esc` | 4 | PunchOK×4, DeliverAny×2 | RecvOther→#52×4, PunchOK→#58×4 | LEGAL-TRANSIENT |
| 54 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading H- - in[-] esc` | 4 | RecvOther×4 | DeliverAny→#9×4 | LEGAL-TRANSIENT |
| 55 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading H- - in[COMPLETE] esc` | 4 | PunchOK×4 | RecvOther→#54×4, DeliverAny→#10×4 | LEGAL-TRANSIENT |
| 56 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading HA - in[-] -` | 4 | RecvOther×4 | PunchOK→#29×4, Tick→#9×2 | LEGAL-TRANSIENT |
| 57 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading HA - in[COMPLETE] -` | 4 | PunchOK×4 | RecvOther→#56×4, PunchOK→#59×4, Tick→#10×2 | LEGAL-TRANSIENT |
| 58 | `A:Committing/upgrading HA path in[COMPLETE] - | B:Committing/upgrading HA path in[COMPLETE] esc` | 4 | PunchOK×8 | RecvOther→#31×4, RecvOther→#27×4 | LEGAL-TRANSIENT |
| 59 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Committing/upgrading HA path in[COMPLETE] -` | 4 | PunchOK×8 | RecvOther→#32×4, RecvOther→#29×4 | LEGAL-TRANSIENT |
| 60 | `A:Punching/upgrading H- - in[-] esc | B:Committing/upgrading HA path in[-] esc` | 4 | RecvOther×4 | DeliverAny→#12×4 | LEGAL-TRANSIENT |
| 61 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading HA - in[-] esc` | 4 | DeliverAny×4 | PunchOK→#62×4, DeliverAny→#13×4 | LEGAL-TRANSIENT |
| 62 | `A:Punching/upgrading H- - in[COMPLETE] esc | B:Committing/upgrading HA path in[-] esc` | 4 | PunchOK×4 | RecvOther→#60×4, DeliverAny→#14×4 | LEGAL-TRANSIENT |
| 63 | `A:Punching/upgrading HA - in[-] - | B:Committing/upgrading HA path in[-] esc` | 4 | RecvOther×4 | PunchOK→#31×4, Tick→#12×2 | LEGAL-TRANSIENT |
| 64 | `A:Punching/upgrading HA - in[-] - | B:Punching/upgrading HA - in[-] esc` | 4 | DeliverAny×2 | PunchOK→#53×4, PunchOK→#68×4, Tick→#13×2 | LEGAL-TRANSIENT |
| 65 | `A:Punching/upgrading HA - in[-] esc | B:Committing/upgrading HA path in[-] -` | 4 | RecvOther×4, DeliverAny×2 | PunchOK→#32×4 | LEGAL-TRANSIENT |
| 66 | `A:Punching/upgrading HA - in[-] esc | B:Punching/upgrading H- - in[-] esc` | 4 | DeliverAny×4 | PunchOK→#55×4, DeliverAny→#13×4 | LEGAL-TRANSIENT |
| 67 | `A:Punching/upgrading HA - in[-] esc | B:Punching/upgrading HA - in[-] -` | 4 | DeliverAny×2 | PunchOK→#57×4, PunchOK→#69×4, Tick→#13×2 | LEGAL-TRANSIENT |
| 68 | `A:Punching/upgrading HA - in[COMPLETE] - | B:Committing/upgrading HA path in[-] esc` | 4 | PunchOK×4 | RecvOther→#63×4, PunchOK→#58×4, Tick→#14×2 | LEGAL-TRANSIENT |
| 69 | `A:Punching/upgrading HA - in[COMPLETE] esc | B:Committing/upgrading HA path in[-] -` | 4 | PunchOK×4, DeliverAny×2 | RecvOther→#65×4, PunchOK→#59×4 | LEGAL-TRANSIENT |
| 70 | `A:AwaitingReady/upgrading -- - in[READY] - | B:Punching/upgrading -- - in[-] -` | 3 | RecvOther×3, ProbeAnswer×1 | RecvOther→#35×3, Tick→#80×1 | LEGAL-TRANSIENT |
| 71 | `A:Failed/relayed -A - in[-] esc | B:Failed/relayed H- - in[-] -` | 3 | RecvOther×3 | - | LEGAL-TERMINAL RelayedOnly |
| 72 | `A:Failed/relayed -A - in[-] esc | B:Punching/upgrading H- - in[REJECT] -` | 3 | PunchFail×2 | RecvOther→#71×3, Tick→#38×1 | LEGAL-TRANSIENT |
| 73 | `A:Failed/relayed H- - in[-] - | B:Failed/relayed -A - in[-] esc` | 3 | RecvOther×3 | - | LEGAL-TERMINAL RelayedOnly |
| 74 | `A:Proposing/upgrading -- - in[ACCEPT,READY] - | B:Punching/upgrading -- - in[-] -` | 3 | ProbeAnswer×1 | RecvOther→#70×3, Tick→#103×1 | LEGAL-TRANSIENT |
| 75 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading -- - in[-] esc` | 3 | RecvOther×2, Tick×1 | DeliverAny→#117×1 | LEGAL-TRANSIENT |
| 76 | `A:Punching/upgrading -- - in[-] esc | B:Punching/upgrading -- - in[-] -` | 3 | Tick×1 | DeliverAny→#122×1 | LEGAL-TRANSIENT |
| 77 | `A:Punching/upgrading -A - in[-] - | B:Punching/upgrading H- - in[-] esc` | 3 | Tick×1, DeliverAny×1 | DeliverAny→#108×2, Tick→#25×1, PunchFail→#119×1 | LEGAL-TRANSIENT |
| 78 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading -A - in[-] -` | 3 | Tick×1, DeliverAny×1 | DeliverAny→#106×2, Tick→#26×1, PunchFail→#112×1 | LEGAL-TRANSIENT |
| 79 | `A:Punching/upgrading H- - in[REJECT] - | B:Failed/relayed -A - in[-] esc` | 3 | PunchFail×2 | RecvOther→#73×3, Tick→#50×1 | LEGAL-TRANSIENT |
| 80 | `A:AwaitingReady/upgrading -- - in[READY] - | B:Punching/upgrading -- - in[-] esc` | 2 | RecvOther×2, Tick×1 | RecvOther→#75×2 | LEGAL-TRANSIENT |
| 81 | `A:Committing/upgrading HA path in[-] - | B:Punching/upgrading H- - in[-] esc` | 2 | RecvOther×2 | DeliverAny→#52×2 | LEGAL-TRANSIENT |
| 82 | `A:Committing/upgrading HA path in[-] - | B:Punching/upgrading H- - in[COMPLETE] esc` | 2 | PunchOK×2 | RecvOther→#81×2, DeliverAny→#53×2 | LEGAL-TRANSIENT |
| 83 | `A:Committing/upgrading HA path in[REJECT] - | B:Failed/relayed H- - in[-] esc` | 2 | RecvOther×2 | RecvOther→#101×2 | LEGAL-TRANSIENT |
| 84 | `A:Committing/upgrading HA path in[REJECT] - | B:Failed/relayed H- - in[COMPLETE] esc` | 2 | PunchOK×2 | RecvOther→#102×2, RecvOther→#83×2 | LEGAL-TRANSIENT |
| 85 | `A:Failed/relayed -A - in[-] - | B:Failed/relayed H- - in[-] esc` | 2 | RecvOther×1 | - | LEGAL-TERMINAL RelayedOnly |
| 86 | `A:Failed/relayed -A - in[-] esc | B:Failed/relayed H- - in[REJECT] esc` | 2 | RecvOther×2 | RecvOther→#23×2 | LEGAL-TRANSIENT |
| 87 | `A:Failed/relayed -A - in[REJECT] esc | B:Failed/relayed H- - in[-] esc` | 2 | RecvOther×2 | RecvOther→#23×2 | LEGAL-TRANSIENT |
| 88 | `A:Failed/relayed -A - in[REJECT] esc | B:Failed/relayed H- - in[REJECT] esc` | 2 | PunchFail×2 | RecvOther→#86×2, RecvOther→#87×2 | LEGAL-TRANSIENT |
| 89 | `A:Failed/relayed H- - in[-] esc | B:Committing/upgrading HA path in[REJECT] -` | 2 | RecvOther×2 | RecvOther→#93×2 | LEGAL-TRANSIENT |
| 90 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed -A - in[-] -` | 2 | RecvOther×1 | - | LEGAL-TERMINAL RelayedOnly |
| 91 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed -A - in[REJECT] esc` | 2 | RecvOther×2 | RecvOther→#24×2 | LEGAL-TRANSIENT |
| 92 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed HA - in[-] -` | 2 | RecvOther×2 | - | LEGAL-TERMINAL RelayedOnly |
| 93 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed HA path in[-] -` | 2 | RecvOther×4 | - | LEGAL-TERMINAL RelayedOnly |
| 94 | `A:Failed/relayed H- - in[-] esc | B:Punching/upgrading -A - in[REJECT] esc` | 2 | PunchFail×2 | RecvOther→#24×2, DeliverAny→#42×2 | LEGAL-TRANSIENT |
| 95 | `A:Failed/relayed H- - in[-] esc | B:Punching/upgrading HA - in[REJECT] -` | 2 | DeliverAny×1 | RecvOther→#92×2, PunchOK→#96×2, Tick→#42×1 | LEGAL-TRANSIENT |
| 96 | `A:Failed/relayed H- - in[COMPLETE] esc | B:Committing/upgrading HA path in[REJECT] -` | 2 | PunchOK×2 | RecvOther→#89×2, RecvOther→#97×2 | LEGAL-TRANSIENT |
| 97 | `A:Failed/relayed H- - in[COMPLETE] esc | B:Failed/relayed HA path in[-] -` | 2 | RecvOther×2 | RecvOther→#93×2 | LEGAL-TRANSIENT |
| 98 | `A:Failed/relayed H- - in[REJECT] esc | B:Failed/relayed -A - in[-] esc` | 2 | RecvOther×2 | RecvOther→#24×2 | LEGAL-TRANSIENT |
| 99 | `A:Failed/relayed H- - in[REJECT] esc | B:Failed/relayed -A - in[REJECT] esc` | 2 | PunchFail×2 | RecvOther→#91×2, RecvOther→#98×2 | LEGAL-TRANSIENT |
| 100 | `A:Failed/relayed HA - in[-] - | B:Failed/relayed H- - in[-] esc` | 2 | RecvOther×2 | - | LEGAL-TERMINAL RelayedOnly |
| 101 | `A:Failed/relayed HA path in[-] - | B:Failed/relayed H- - in[-] esc` | 2 | RecvOther×4 | - | LEGAL-TERMINAL RelayedOnly |
| 102 | `A:Failed/relayed HA path in[-] - | B:Failed/relayed H- - in[COMPLETE] esc` | 2 | RecvOther×2 | RecvOther→#101×2 | LEGAL-TRANSIENT |
| 103 | `A:Proposing/upgrading -- - in[ACCEPT,READY] - | B:Punching/upgrading -- - in[-] esc` | 2 | Tick×1 | RecvOther→#80×2 | LEGAL-TRANSIENT |
| 104 | `A:Punching/upgrading -A - in[REJECT] esc | B:Failed/relayed H- - in[-] esc` | 2 | PunchFail×2 | RecvOther→#23×2, DeliverAny→#51×2 | LEGAL-TRANSIENT |
| 105 | `A:Punching/upgrading H- - in[-] esc | B:Committing/upgrading HA path in[-] -` | 2 | RecvOther×2 | DeliverAny→#65×2 | LEGAL-TRANSIENT |
| 106 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading HA - in[-] -` | 2 | DeliverAny×2 | PunchOK→#107×2, DeliverAny→#67×2 | LEGAL-TRANSIENT |
| 107 | `A:Punching/upgrading H- - in[COMPLETE] esc | B:Committing/upgrading HA path in[-] -` | 2 | PunchOK×2 | RecvOther→#105×2, DeliverAny→#69×2 | LEGAL-TRANSIENT |
| 108 | `A:Punching/upgrading HA - in[-] - | B:Punching/upgrading H- - in[-] esc` | 2 | DeliverAny×2 | PunchOK→#82×2, DeliverAny→#64×2 | LEGAL-TRANSIENT |
| 109 | `A:Punching/upgrading HA - in[REJECT] - | B:Failed/relayed H- - in[-] esc` | 2 | DeliverAny×1 | RecvOther→#100×2, PunchOK→#84×2, Tick→#51×1 | LEGAL-TRANSIENT |
| 110 | `A:AwaitingReady/upgrading -- - in[-] - | B:Discovering/upgrading -- - in[-] -` | 1 | RecvOther×1 | ProbeAnswer→#70×1 | LEGAL-TRANSIENT |
| 111 | `A:Discovering/upgrading -- - in[-] - | B:Idle/relayed -- - in[-] -` | 1 | Propose×1 | ProbeAnswer→#114×1 | LEGAL-TRANSIENT |
| 112 | `A:Failed/relayed H- - in[-] esc | B:Punching/upgrading -A - in[REJECT] -` | 1 | PunchFail×1 | RecvOther→#90×1, DeliverAny→#95×1 | LEGAL-TRANSIENT |
| 113 | `A:Idle/relayed -- - in[-] - | B:Idle/relayed -- - in[-] -` | 1 | - | Propose→#111×1 | LEGAL-TRANSIENT |
| 114 | `A:Proposing/upgrading -- - in[-] - | B:Idle/relayed -- - in[REQUEST] -` | 1 | ProbeAnswer×1 | RecvRequest→#115×1 | LEGAL-TRANSIENT |
| 115 | `A:Proposing/upgrading -- - in[ACCEPT] - | B:Discovering/upgrading -- - in[-] -` | 1 | RecvRequest×1 | RecvOther→#110×1, ProbeAnswer→#74×1 | LEGAL-TRANSIENT |
| 116 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading H- - in[-] -` | 1 | DeliverAny×1 | DeliverAny→#118×1 | LEGAL-TRANSIENT |
| 117 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading H- - in[-] esc` | 1 | DeliverAny×1 | DeliverAny→#77×1 | LEGAL-TRANSIENT |
| 118 | `A:Punching/upgrading -A - in[-] - | B:Punching/upgrading H- - in[-] -` | 1 | DeliverAny×1 | Tick→#48×1, Tick→#77×1 | LEGAL-TRANSIENT |
| 119 | `A:Punching/upgrading -A - in[REJECT] - | B:Failed/relayed H- - in[-] esc` | 1 | PunchFail×1 | RecvOther→#85×1, DeliverAny→#109×1 | LEGAL-TRANSIENT |
| 120 | `A:Punching/upgrading H- - in[-] - | B:Punching/upgrading -- - in[-] -` | 1 | DeliverAny×1 | DeliverAny→#121×1 | LEGAL-TRANSIENT |
| 121 | `A:Punching/upgrading H- - in[-] - | B:Punching/upgrading -A - in[-] -` | 1 | DeliverAny×1 | Tick→#78×1, Tick→#49×1 | LEGAL-TRANSIENT |
| 122 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading -- - in[-] -` | 1 | DeliverAny×1 | DeliverAny→#78×1 | LEGAL-TRANSIENT |

Classes with no path to a terminal state, or a rejected terminal: 0.
