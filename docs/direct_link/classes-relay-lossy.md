### State classes: relay-lossy

2016 states, 5312 transitions, 241 classes. Class: per side `phase/linkPath heard-acked path in[signals in flight] flags`.

| # | class (A \| B) | states | in (by action) | out (action -> class) | verdict |
|---|---|---|---|---|---|
| 1 | `A:Connected/direct HA path in[-] esc | B:Connected/direct HA path in[-] esc` | 60 | RecvOther×80 | - | LEGAL-TERMINAL DirectPathUp |
| 2 | `A:Committing/upgrading HA path in[-] esc | B:Connected/direct HA path in[-] esc` | 40 | LoseSignal×40, RecvOther×20 | Timeout→#11×80 | LEGAL-TRANSIENT |
| 3 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading HA - in[-] esc` | 40 | RecvOther×20, LoseSignal×20, DeliverAny×8, Tick×4 | Timeout→#13×40, PunchOK→#4×20, PunchOK→#23×20 | LEGAL-TRANSIENT |
| 4 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Connected/direct HA path in[-] esc` | 40 | PunchOK×20, RecvOther×20 | RecvOther→#1×40, LoseSignal→#2×40 | LEGAL-TRANSIENT |
| 5 | `A:Connected/direct HA path in[-] esc | B:Committing/upgrading HA path in[-] esc` | 40 | LoseSignal×40, RecvOther×20 | Timeout→#7×80 | LEGAL-TRANSIENT |
| 6 | `A:Connected/direct HA path in[-] esc | B:Committing/upgrading HA path in[COMPLETE] esc` | 40 | RecvOther×20, PunchOK×20 | RecvOther→#1×40, LoseSignal→#5×40 | LEGAL-TRANSIENT |
| 7 | `A:Connected/direct HA path in[-] esc | B:Failed/relayed HA path in[-] esc` | 40 | Timeout×80, RecvOther×60, LoseSignal×40 | FallBack→#9×40 | LEGAL-TRANSIENT |
| 8 | `A:Connected/direct HA path in[-] esc | B:Failed/relayed HA path in[COMPLETE] esc` | 40 | PunchOK×20, RecvOther×20 | FallBack→#10×40, RecvOther→#7×40, LoseSignal→#7×40 | LEGAL-TRANSIENT |
| 9 | `A:Connected/relayed HA path in[-] esc | B:Failed/relayed HA path in[-] esc` | 40 | FallBack×40, RecvOther×40, LoseSignal×40 | - | LEGAL-TERMINAL RelayedOnly |
| 10 | `A:Connected/relayed HA path in[-] esc | B:Failed/relayed HA path in[COMPLETE] esc` | 40 | FallBack×40 | RecvOther→#9×40, LoseSignal→#9×40 | LEGAL-TRANSIENT |
| 11 | `A:Failed/relayed HA path in[-] esc | B:Connected/direct HA path in[-] esc` | 40 | Timeout×80, RecvOther×60, LoseSignal×40 | FallBack→#12×40 | LEGAL-TRANSIENT |
| 12 | `A:Failed/relayed HA path in[-] esc | B:Connected/relayed HA path in[-] esc` | 40 | FallBack×40, RecvOther×40, LoseSignal×40 | - | LEGAL-TERMINAL RelayedOnly |
| 13 | `A:Failed/relayed HA path in[-] esc | B:Punching/upgrading HA - in[-] esc` | 40 | Timeout×40, RecvOther×20, LoseSignal×20, DeliverAny×8 | PunchOK→#14×20, PunchOK→#32×20 | LEGAL-TRANSIENT |
| 14 | `A:Failed/relayed HA path in[COMPLETE] esc | B:Connected/direct HA path in[-] esc` | 40 | PunchOK×20, RecvOther×20 | RecvOther→#11×40, LoseSignal→#11×40, FallBack→#15×40 | LEGAL-TRANSIENT |
| 15 | `A:Failed/relayed HA path in[COMPLETE] esc | B:Connected/relayed HA path in[-] esc` | 40 | FallBack×40 | RecvOther→#12×40, LoseSignal→#12×40 | LEGAL-TRANSIENT |
| 16 | `A:Punching/upgrading HA - in[-] esc | B:Committing/upgrading HA path in[-] esc` | 40 | RecvOther×20, LoseSignal×20, DeliverAny×8, Tick×4 | Timeout→#17×40, PunchOK→#6×20, PunchOK→#19×20 | LEGAL-TRANSIENT |
| 17 | `A:Punching/upgrading HA - in[-] esc | B:Failed/relayed HA path in[-] esc` | 40 | Timeout×40, RecvOther×20, LoseSignal×20, DeliverAny×8 | PunchOK→#8×20, PunchOK→#21×20 | LEGAL-TRANSIENT |
| 18 | `A:Committing/upgrading HA path in[-] esc | B:Committing/upgrading HA path in[-] esc` | 20 | LoseSignal×40 | Timeout→#27×40, Timeout→#20×40 | LEGAL-TRANSIENT |
| 19 | `A:Committing/upgrading HA path in[-] esc | B:Committing/upgrading HA path in[COMPLETE] esc` | 20 | LoseSignal×20, PunchOK×20 | Timeout→#28×40, RecvOther→#2×20, LoseSignal→#18×20 | LEGAL-TRANSIENT |
| 20 | `A:Committing/upgrading HA path in[-] esc | B:Failed/relayed HA path in[-] esc` | 20 | Timeout×40, LoseSignal×40, RecvOther×20 | Timeout→#29×40 | LEGAL-TRANSIENT |
| 21 | `A:Committing/upgrading HA path in[-] esc | B:Failed/relayed HA path in[COMPLETE] esc` | 20 | PunchOK×20, LoseSignal×20 | Timeout→#30×40, RecvOther→#20×20, LoseSignal→#20×20 | LEGAL-TRANSIENT |
| 22 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading HA - in[COMPLETE] esc` | 20 | PunchOK×20, DeliverAny×4, Tick×2 | Timeout→#31×20, RecvOther→#3×20, PunchOK→#24×20, LoseSignal→#3×20 | LEGAL-TRANSIENT |
| 23 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Committing/upgrading HA path in[-] esc` | 20 | LoseSignal×20, PunchOK×20 | Timeout→#25×40, RecvOther→#5×20, LoseSignal→#18×20 | LEGAL-TRANSIENT |
| 24 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Committing/upgrading HA path in[COMPLETE] esc` | 20 | PunchOK×40 | RecvOther→#6×20, LoseSignal→#19×20, RecvOther→#4×20, LoseSignal→#23×20 | LEGAL-TRANSIENT |
| 25 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Failed/relayed HA path in[-] esc` | 20 | Timeout×40, RecvOther×20, LoseSignal×20 | RecvOther→#7×20, LoseSignal→#20×20 | LEGAL-TRANSIENT |
| 26 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Failed/relayed HA path in[COMPLETE] esc` | 20 | PunchOK×20 | RecvOther→#8×20, LoseSignal→#21×20, RecvOther→#25×20, LoseSignal→#25×20 | LEGAL-TRANSIENT |
| 27 | `A:Failed/relayed HA path in[-] esc | B:Committing/upgrading HA path in[-] esc` | 20 | LoseSignal×40, Timeout×40, RecvOther×20 | Timeout→#29×40 | LEGAL-TRANSIENT |
| 28 | `A:Failed/relayed HA path in[-] esc | B:Committing/upgrading HA path in[COMPLETE] esc` | 20 | Timeout×40, RecvOther×20, LoseSignal×20 | RecvOther→#11×20, LoseSignal→#27×20 | LEGAL-TRANSIENT |
| 29 | `A:Failed/relayed HA path in[-] esc | B:Failed/relayed HA path in[-] esc` | 20 | Timeout×80, RecvOther×40, LoseSignal×40 | - | LEGAL-TERMINAL RelayedOnly |
| 30 | `A:Failed/relayed HA path in[-] esc | B:Failed/relayed HA path in[COMPLETE] esc` | 20 | Timeout×40 | RecvOther→#29×20, LoseSignal→#29×20 | LEGAL-TRANSIENT |
| 31 | `A:Failed/relayed HA path in[-] esc | B:Punching/upgrading HA - in[COMPLETE] esc` | 20 | Timeout×20, DeliverAny×4, Tick×2 | RecvOther→#13×20, PunchOK→#33×20, LoseSignal→#13×20 | LEGAL-TRANSIENT |
| 32 | `A:Failed/relayed HA path in[COMPLETE] esc | B:Committing/upgrading HA path in[-] esc` | 20 | PunchOK×20, LoseSignal×20 | Timeout→#34×40, RecvOther→#27×20, LoseSignal→#27×20 | LEGAL-TRANSIENT |
| 33 | `A:Failed/relayed HA path in[COMPLETE] esc | B:Committing/upgrading HA path in[COMPLETE] esc` | 20 | PunchOK×20 | RecvOther→#28×20, LoseSignal→#28×20, RecvOther→#14×20, LoseSignal→#32×20 | LEGAL-TRANSIENT |
| 34 | `A:Failed/relayed HA path in[COMPLETE] esc | B:Failed/relayed HA path in[-] esc` | 20 | Timeout×40 | RecvOther→#29×20, LoseSignal→#29×20 | LEGAL-TRANSIENT |
| 35 | `A:Punching/upgrading HA - in[-] esc | B:Punching/upgrading HA - in[-] esc` | 20 | DeliverAny×8, Tick×4 | PunchOK→#22×20, PunchOK→#36×20 | LEGAL-TRANSIENT |
| 36 | `A:Punching/upgrading HA - in[COMPLETE] esc | B:Committing/upgrading HA path in[-] esc` | 20 | PunchOK×20, DeliverAny×4, Tick×2 | RecvOther→#16×20, PunchOK→#24×20, LoseSignal→#16×20, Timeout→#37×20 | LEGAL-TRANSIENT |
| 37 | `A:Punching/upgrading HA - in[COMPLETE] esc | B:Failed/relayed HA path in[-] esc` | 20 | Timeout×20, DeliverAny×4, Tick×2 | RecvOther→#17×20, PunchOK→#26×20, LoseSignal→#17×20 | LEGAL-TRANSIENT |
| 38 | `A:Connected/direct HA path in[-] - | B:Connected/direct HA path in[-] esc` | 12 | RecvOther×16 | - | LEGAL-TERMINAL DirectPathUp |
| 39 | `A:Connected/direct HA path in[-] esc | B:Connected/direct HA path in[-] -` | 12 | RecvOther×16 | - | LEGAL-TERMINAL DirectPathUp |
| 40 | `A:Failed/relayed -A - in[-] esc | B:Failed/relayed H- - in[-] esc` | 12 | RecvOther×12, LoseSignal×4 | - | LEGAL-TERMINAL RelayedOnly |
| 41 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed -A - in[-] esc` | 12 | RecvOther×12, LoseSignal×4 | - | LEGAL-TERMINAL RelayedOnly |
| 42 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed HA path in[-] esc` | 12 | RecvOther×18, LoseSignal×12, Timeout×12 | - | LEGAL-TERMINAL RelayedOnly |
| 43 | `A:Failed/relayed H- - in[COMPLETE] esc | B:Failed/relayed HA path in[-] esc` | 12 | Timeout×12, RecvOther×6 | RecvOther→#42×12, LoseSignal→#42×12 | LEGAL-TRANSIENT |
| 44 | `A:Failed/relayed HA path in[-] esc | B:Failed/relayed H- - in[-] esc` | 12 | RecvOther×18, LoseSignal×12, Timeout×12 | - | LEGAL-TERMINAL RelayedOnly |
| 45 | `A:Failed/relayed HA path in[-] esc | B:Failed/relayed H- - in[COMPLETE] esc` | 12 | Timeout×12, RecvOther×6 | RecvOther→#44×12, LoseSignal→#44×12 | LEGAL-TRANSIENT |
| 46 | `A:Punching/upgrading -A - in[-] esc | B:Punching/upgrading H- - in[-] esc` | 10 | Tick×3 | DeliverAny→#156×4, PunchFail→#90×2, PunchFail→#214×2 | LEGAL-TRANSIENT |
| 47 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading -A - in[-] esc` | 10 | Tick×3 | DeliverAny→#152×4, PunchFail→#201×2, PunchFail→#102×2 | LEGAL-TRANSIENT |
| 48 | `A:Committing/upgrading HA path in[-] - | B:Connected/direct HA path in[-] esc` | 8 | LoseSignal×8, RecvOther×4 | Timeout→#67×16 | LEGAL-TRANSIENT |
| 49 | `A:Committing/upgrading HA path in[-] - | B:Punching/upgrading HA - in[-] esc` | 8 | DeliverAny×4, RecvOther×4, LoseSignal×4 | Timeout→#69×8, PunchOK→#53×4, PunchOK→#117×4 | LEGAL-TRANSIENT |
| 50 | `A:Committing/upgrading HA path in[-] esc | B:Connected/direct HA path in[-] -` | 8 | LoseSignal×8, RecvOther×4 | Timeout→#70×16 | LEGAL-TRANSIENT |
| 51 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading H- - in[-] esc` | 8 | RecvOther×4, LoseSignal×4 | Timeout→#72×8, DeliverAny→#3×8 | LEGAL-TRANSIENT |
| 52 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading HA - in[-] -` | 8 | RecvOther×4, LoseSignal×4 | Timeout→#73×8, Tick→#3×4, PunchOK→#54×4, PunchOK→#121×4 | LEGAL-TRANSIENT |
| 53 | `A:Committing/upgrading HA path in[COMPLETE] - | B:Connected/direct HA path in[-] esc` | 8 | PunchOK×4, RecvOther×4 | RecvOther→#38×8, LoseSignal→#48×8 | LEGAL-TRANSIENT |
| 54 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Connected/direct HA path in[-] -` | 8 | PunchOK×4, RecvOther×4 | RecvOther→#39×8, LoseSignal→#50×8 | LEGAL-TRANSIENT |
| 55 | `A:Connected/direct HA path in[-] - | B:Committing/upgrading HA path in[-] esc` | 8 | LoseSignal×8, RecvOther×4 | Timeout→#57×16 | LEGAL-TRANSIENT |
| 56 | `A:Connected/direct HA path in[-] - | B:Committing/upgrading HA path in[COMPLETE] esc` | 8 | RecvOther×4, PunchOK×4 | RecvOther→#38×8, LoseSignal→#55×8 | LEGAL-TRANSIENT |
| 57 | `A:Connected/direct HA path in[-] - | B:Failed/relayed HA path in[-] esc` | 8 | Timeout×16, RecvOther×12, LoseSignal×8 | FallBack→#63×8 | LEGAL-TRANSIENT |
| 58 | `A:Connected/direct HA path in[-] - | B:Failed/relayed HA path in[COMPLETE] esc` | 8 | PunchOK×4, RecvOther×4 | FallBack→#64×8, RecvOther→#57×8, LoseSignal→#57×8 | LEGAL-TRANSIENT |
| 59 | `A:Connected/direct HA path in[-] esc | B:Committing/upgrading HA path in[-] -` | 8 | LoseSignal×8, RecvOther×4 | Timeout→#61×16 | LEGAL-TRANSIENT |
| 60 | `A:Connected/direct HA path in[-] esc | B:Committing/upgrading HA path in[COMPLETE] -` | 8 | PunchOK×4, RecvOther×4 | RecvOther→#39×8, LoseSignal→#59×8 | LEGAL-TRANSIENT |
| 61 | `A:Connected/direct HA path in[-] esc | B:Failed/relayed HA path in[-] -` | 8 | Timeout×16, RecvOther×12, LoseSignal×8 | FallBack→#65×8 | LEGAL-TRANSIENT |
| 62 | `A:Connected/direct HA path in[-] esc | B:Failed/relayed HA path in[COMPLETE] -` | 8 | PunchOK×4, RecvOther×4 | FallBack→#66×8, RecvOther→#61×8, LoseSignal→#61×8 | LEGAL-TRANSIENT |
| 63 | `A:Connected/relayed HA path in[-] - | B:Failed/relayed HA path in[-] esc` | 8 | FallBack×8, RecvOther×8, LoseSignal×8 | - | LEGAL-TERMINAL RelayedOnly |
| 64 | `A:Connected/relayed HA path in[-] - | B:Failed/relayed HA path in[COMPLETE] esc` | 8 | FallBack×8 | RecvOther→#63×8, LoseSignal→#63×8 | LEGAL-TRANSIENT |
| 65 | `A:Connected/relayed HA path in[-] esc | B:Failed/relayed HA path in[-] -` | 8 | RecvOther×8, LoseSignal×8, FallBack×8 | - | LEGAL-TERMINAL RelayedOnly |
| 66 | `A:Connected/relayed HA path in[-] esc | B:Failed/relayed HA path in[COMPLETE] -` | 8 | FallBack×8 | RecvOther→#65×8, LoseSignal→#65×8 | LEGAL-TRANSIENT |
| 67 | `A:Failed/relayed HA path in[-] - | B:Connected/direct HA path in[-] esc` | 8 | Timeout×16, RecvOther×12, LoseSignal×8 | FallBack→#68×8 | LEGAL-TRANSIENT |
| 68 | `A:Failed/relayed HA path in[-] - | B:Connected/relayed HA path in[-] esc` | 8 | FallBack×8, RecvOther×8, LoseSignal×8 | - | LEGAL-TERMINAL RelayedOnly |
| 69 | `A:Failed/relayed HA path in[-] - | B:Punching/upgrading HA - in[-] esc` | 8 | Timeout×8, DeliverAny×4, RecvOther×4, LoseSignal×4 | PunchOK→#74×4, PunchOK→#144×4 | LEGAL-TRANSIENT |
| 70 | `A:Failed/relayed HA path in[-] esc | B:Connected/direct HA path in[-] -` | 8 | Timeout×16, RecvOther×12, LoseSignal×8 | FallBack→#71×8 | LEGAL-TRANSIENT |
| 71 | `A:Failed/relayed HA path in[-] esc | B:Connected/relayed HA path in[-] -` | 8 | FallBack×8, RecvOther×8, LoseSignal×8 | - | LEGAL-TERMINAL RelayedOnly |
| 72 | `A:Failed/relayed HA path in[-] esc | B:Punching/upgrading H- - in[-] esc` | 8 | Timeout×8, RecvOther×4, LoseSignal×4 | DeliverAny→#13×8 | LEGAL-TRANSIENT |
| 73 | `A:Failed/relayed HA path in[-] esc | B:Punching/upgrading HA - in[-] -` | 8 | Timeout×8, RecvOther×4, LoseSignal×4 | Tick→#13×4, PunchOK→#76×4, PunchOK→#147×4 | LEGAL-TRANSIENT |
| 74 | `A:Failed/relayed HA path in[COMPLETE] - | B:Connected/direct HA path in[-] esc` | 8 | PunchOK×4, RecvOther×4 | RecvOther→#67×8, LoseSignal→#67×8, FallBack→#75×8 | LEGAL-TRANSIENT |
| 75 | `A:Failed/relayed HA path in[COMPLETE] - | B:Connected/relayed HA path in[-] esc` | 8 | FallBack×8 | RecvOther→#68×8, LoseSignal→#68×8 | LEGAL-TRANSIENT |
| 76 | `A:Failed/relayed HA path in[COMPLETE] esc | B:Connected/direct HA path in[-] -` | 8 | PunchOK×4, RecvOther×4 | RecvOther→#70×8, LoseSignal→#70×8, FallBack→#77×8 | LEGAL-TRANSIENT |
| 77 | `A:Failed/relayed HA path in[COMPLETE] esc | B:Connected/relayed HA path in[-] -` | 8 | FallBack×8 | RecvOther→#71×8, LoseSignal→#71×8 | LEGAL-TRANSIENT |
| 78 | `A:Punching/upgrading H- - in[-] esc | B:Committing/upgrading HA path in[-] esc` | 8 | RecvOther×4, LoseSignal×4 | Timeout→#79×8, DeliverAny→#16×8 | LEGAL-TRANSIENT |
| 79 | `A:Punching/upgrading H- - in[-] esc | B:Failed/relayed HA path in[-] esc` | 8 | Timeout×8, RecvOther×4, LoseSignal×4 | DeliverAny→#17×8 | LEGAL-TRANSIENT |
| 80 | `A:Punching/upgrading HA - in[-] - | B:Committing/upgrading HA path in[-] esc` | 8 | RecvOther×4, LoseSignal×4 | Timeout→#81×8, Tick→#16×4, PunchOK→#56×4, PunchOK→#106×4 | LEGAL-TRANSIENT |
| 81 | `A:Punching/upgrading HA - in[-] - | B:Failed/relayed HA path in[-] esc` | 8 | Timeout×8, RecvOther×4, LoseSignal×4 | Tick→#17×4, PunchOK→#58×4, PunchOK→#108×4 | LEGAL-TRANSIENT |
| 82 | `A:Punching/upgrading HA - in[-] esc | B:Committing/upgrading HA path in[-] -` | 8 | DeliverAny×4, RecvOther×4, LoseSignal×4 | Timeout→#83×8, PunchOK→#60×4, PunchOK→#112×4 | LEGAL-TRANSIENT |
| 83 | `A:Punching/upgrading HA - in[-] esc | B:Failed/relayed HA path in[-] -` | 8 | Timeout×8, DeliverAny×4, RecvOther×4, LoseSignal×4 | PunchOK→#62×4, PunchOK→#114×4 | LEGAL-TRANSIENT |
| 84 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading -- - in[-] -` | 7 | RecvOther×3 | Tick→#175×1, Tick→#174×1, DeliverAny→#234×1, DeliverAny→#239×1 | LEGAL-TRANSIENT |
| 85 | `A:Committing/upgrading HA path in[-] esc | B:Failed/relayed H- - in[-] esc` | 6 | LoseSignal×12, RecvOther×6 | Timeout→#44×12 | LEGAL-TRANSIENT |
| 86 | `A:Committing/upgrading HA path in[-] esc | B:Failed/relayed H- - in[COMPLETE] esc` | 6 | PunchOK×6, LoseSignal×6 | Timeout→#45×12, RecvOther→#85×6, LoseSignal→#85×6 | LEGAL-TRANSIENT |
| 87 | `A:Committing/upgrading HA path in[REJECT] esc | B:Failed/relayed H- - in[-] esc` | 6 | RecvOther×6, LoseSignal×6 | RecvOther→#44×6, LoseSignal→#85×6 | LEGAL-TRANSIENT |
| 88 | `A:Committing/upgrading HA path in[REJECT] esc | B:Failed/relayed H- - in[COMPLETE] esc` | 6 | PunchOK×6 | RecvOther→#45×6, LoseSignal→#86×6, RecvOther→#87×6, LoseSignal→#87×6 | LEGAL-TRANSIENT |
| 89 | `A:Failed/relayed -A - in[-] esc | B:Punching/upgrading H- - in[-] esc` | 6 | LoseSignal×6, Tick×1 | PunchFail→#193×2 | LEGAL-TRANSIENT |
| 90 | `A:Failed/relayed -A - in[-] esc | B:Punching/upgrading H- - in[REJECT] esc` | 6 | PunchFail×2, Tick×1 | RecvOther→#40×6, LoseSignal→#89×6, PunchFail→#194×2 | LEGAL-TRANSIENT |
| 91 | `A:Failed/relayed H- - in[-] esc | B:Committing/upgrading HA path in[-] esc` | 6 | LoseSignal×12, RecvOther×6 | Timeout→#42×12 | LEGAL-TRANSIENT |
| 92 | `A:Failed/relayed H- - in[-] esc | B:Committing/upgrading HA path in[REJECT] esc` | 6 | RecvOther×6, LoseSignal×6 | RecvOther→#42×6, LoseSignal→#91×6 | LEGAL-TRANSIENT |
| 93 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed HA - in[-] esc` | 6 | RecvOther×6 | - | LEGAL-TERMINAL RelayedOnly |
| 94 | `A:Failed/relayed H- - in[-] esc | B:Punching/upgrading HA - in[-] esc` | 6 | LoseSignal×6, DeliverAny×2, Tick×1 | PunchOK→#96×6 | LEGAL-TRANSIENT |
| 95 | `A:Failed/relayed H- - in[-] esc | B:Punching/upgrading HA - in[REJECT] esc` | 6 | DeliverAny×2, Tick×1 | RecvOther→#93×6, PunchOK→#97×6, LoseSignal→#94×6 | LEGAL-TRANSIENT |
| 96 | `A:Failed/relayed H- - in[COMPLETE] esc | B:Committing/upgrading HA path in[-] esc` | 6 | PunchOK×6, LoseSignal×6 | Timeout→#43×12, RecvOther→#91×6, LoseSignal→#91×6 | LEGAL-TRANSIENT |
| 97 | `A:Failed/relayed H- - in[COMPLETE] esc | B:Committing/upgrading HA path in[REJECT] esc` | 6 | PunchOK×6 | RecvOther→#92×6, LoseSignal→#92×6, RecvOther→#43×6, LoseSignal→#96×6 | LEGAL-TRANSIENT |
| 98 | `A:Failed/relayed HA - in[-] esc | B:Failed/relayed H- - in[-] esc` | 6 | RecvOther×6 | - | LEGAL-TERMINAL RelayedOnly |
| 99 | `A:Punching/upgrading -A - in[-] esc | B:Punching/upgrading H- - in[-] -` | 6 | Tick×1 | Tick→#46×2, PunchFail→#169×2 | LEGAL-TRANSIENT |
| 100 | `A:Punching/upgrading H- - in[-] - | B:Punching/upgrading -A - in[-] esc` | 6 | Tick×1 | Tick→#47×2, PunchFail→#179×2 | LEGAL-TRANSIENT |
| 101 | `A:Punching/upgrading H- - in[-] esc | B:Failed/relayed -A - in[-] esc` | 6 | LoseSignal×6, Tick×1 | PunchFail→#198×2 | LEGAL-TRANSIENT |
| 102 | `A:Punching/upgrading H- - in[REJECT] esc | B:Failed/relayed -A - in[-] esc` | 6 | PunchFail×2, Tick×1 | RecvOther→#41×6, LoseSignal→#101×6, PunchFail→#207×2 | LEGAL-TRANSIENT |
| 103 | `A:Punching/upgrading HA - in[-] esc | B:Failed/relayed H- - in[-] esc` | 6 | LoseSignal×6, DeliverAny×2, Tick×1 | PunchOK→#86×6 | LEGAL-TRANSIENT |
| 104 | `A:Punching/upgrading HA - in[REJECT] esc | B:Failed/relayed H- - in[-] esc` | 6 | DeliverAny×2, Tick×1 | RecvOther→#98×6, PunchOK→#88×6, LoseSignal→#103×6 | LEGAL-TRANSIENT |
| 105 | `A:Committing/upgrading HA path in[-] - | B:Committing/upgrading HA path in[-] esc` | 4 | LoseSignal×8 | Timeout→#130×8, Timeout→#107×8 | LEGAL-TRANSIENT |
| 106 | `A:Committing/upgrading HA path in[-] - | B:Committing/upgrading HA path in[COMPLETE] esc` | 4 | LoseSignal×4, PunchOK×4 | Timeout→#131×8, RecvOther→#48×4, LoseSignal→#105×4 | LEGAL-TRANSIENT |
| 107 | `A:Committing/upgrading HA path in[-] - | B:Failed/relayed HA path in[-] esc` | 4 | Timeout×8, LoseSignal×8, RecvOther×4 | Timeout→#134×8 | LEGAL-TRANSIENT |
| 108 | `A:Committing/upgrading HA path in[-] - | B:Failed/relayed HA path in[COMPLETE] esc` | 4 | PunchOK×4, LoseSignal×4 | Timeout→#135×8, RecvOther→#107×4, LoseSignal→#107×4 | LEGAL-TRANSIENT |
| 109 | `A:Committing/upgrading HA path in[-] - | B:Punching/upgrading H- - in[-] esc` | 4 | RecvOther×2, LoseSignal×2 | Timeout→#136×4, DeliverAny→#49×4 | LEGAL-TRANSIENT |
| 110 | `A:Committing/upgrading HA path in[-] - | B:Punching/upgrading HA - in[COMPLETE] esc` | 4 | PunchOK×4, DeliverAny×2 | Timeout→#137×4, RecvOther→#49×4, PunchOK→#118×4, LoseSignal→#49×4 | LEGAL-TRANSIENT |
| 111 | `A:Committing/upgrading HA path in[-] esc | B:Committing/upgrading HA path in[-] -` | 4 | LoseSignal×8 | Timeout→#138×8, Timeout→#113×8 | LEGAL-TRANSIENT |
| 112 | `A:Committing/upgrading HA path in[-] esc | B:Committing/upgrading HA path in[COMPLETE] -` | 4 | PunchOK×4, LoseSignal×4 | Timeout→#139×8, RecvOther→#50×4, LoseSignal→#111×4 | LEGAL-TRANSIENT |
| 113 | `A:Committing/upgrading HA path in[-] esc | B:Failed/relayed HA path in[-] -` | 4 | LoseSignal×8, Timeout×8, RecvOther×4 | Timeout→#140×8 | LEGAL-TRANSIENT |
| 114 | `A:Committing/upgrading HA path in[-] esc | B:Failed/relayed HA path in[COMPLETE] -` | 4 | PunchOK×4, LoseSignal×4 | Timeout→#141×8, RecvOther→#113×4, LoseSignal→#113×4 | LEGAL-TRANSIENT |
| 115 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading H- - in[COMPLETE] esc` | 4 | PunchOK×4 | Timeout→#142×4, RecvOther→#51×4, LoseSignal→#51×4, DeliverAny→#22×4 | LEGAL-TRANSIENT |
| 116 | `A:Committing/upgrading HA path in[-] esc | B:Punching/upgrading HA - in[COMPLETE] -` | 4 | PunchOK×4 | Timeout→#143×4, RecvOther→#52×4, PunchOK→#122×4, LoseSignal→#52×4, Tick→#22×2 | LEGAL-TRANSIENT |
| 117 | `A:Committing/upgrading HA path in[COMPLETE] - | B:Committing/upgrading HA path in[-] esc` | 4 | PunchOK×4, LoseSignal×4 | Timeout→#119×8, RecvOther→#55×4, LoseSignal→#105×4 | LEGAL-TRANSIENT |
| 118 | `A:Committing/upgrading HA path in[COMPLETE] - | B:Committing/upgrading HA path in[COMPLETE] esc` | 4 | PunchOK×8 | RecvOther→#56×4, LoseSignal→#106×4, RecvOther→#53×4, LoseSignal→#117×4 | LEGAL-TRANSIENT |
| 119 | `A:Committing/upgrading HA path in[COMPLETE] - | B:Failed/relayed HA path in[-] esc` | 4 | Timeout×8, RecvOther×4, LoseSignal×4 | RecvOther→#57×4, LoseSignal→#107×4 | LEGAL-TRANSIENT |
| 120 | `A:Committing/upgrading HA path in[COMPLETE] - | B:Failed/relayed HA path in[COMPLETE] esc` | 4 | PunchOK×4 | RecvOther→#58×4, LoseSignal→#108×4, RecvOther→#119×4, LoseSignal→#119×4 | LEGAL-TRANSIENT |
| 121 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Committing/upgrading HA path in[-] -` | 4 | LoseSignal×4, PunchOK×4 | Timeout→#123×8, RecvOther→#59×4, LoseSignal→#111×4 | LEGAL-TRANSIENT |
| 122 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Committing/upgrading HA path in[COMPLETE] -` | 4 | PunchOK×8 | RecvOther→#60×4, LoseSignal→#112×4, RecvOther→#54×4, LoseSignal→#121×4 | LEGAL-TRANSIENT |
| 123 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Failed/relayed HA path in[-] -` | 4 | Timeout×8, RecvOther×4, LoseSignal×4 | RecvOther→#61×4, LoseSignal→#113×4 | LEGAL-TRANSIENT |
| 124 | `A:Committing/upgrading HA path in[COMPLETE] esc | B:Failed/relayed HA path in[COMPLETE] -` | 4 | PunchOK×4 | RecvOther→#62×4, LoseSignal→#114×4, RecvOther→#123×4, LoseSignal→#123×4 | LEGAL-TRANSIENT |
| 125 | `A:Failed/relayed -- - in[-] - | B:Punching/upgrading -- - in[-] esc` | 4 | RecvOther×4, LoseSignal×4, Timeout×4, Tick×1 | PunchFail→#190×2 | LEGAL-TRANSIENT |
| 126 | `A:Failed/relayed -- - in[ACCEPT,READY] - | B:Punching/upgrading -- - in[-] esc` | 4 | Tick×1 | RecvOther→#127×4, LoseSignal→#127×4, PunchFail→#188×2 | LEGAL-TRANSIENT |
| 127 | `A:Failed/relayed -- - in[READY] - | B:Punching/upgrading -- - in[-] esc` | 4 | RecvOther×4, LoseSignal×4, Tick×1 | RecvOther→#125×4, LoseSignal→#125×4, PunchFail→#189×2 | LEGAL-TRANSIENT |
| 128 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed HA path in[-] -` | 4 | RecvOther×6, LoseSignal×4, Timeout×4 | - | LEGAL-TERMINAL RelayedOnly |
| 129 | `A:Failed/relayed H- - in[COMPLETE] esc | B:Failed/relayed HA path in[-] -` | 4 | Timeout×4, RecvOther×2 | RecvOther→#128×4, LoseSignal→#128×4 | LEGAL-TRANSIENT |
| 130 | `A:Failed/relayed HA path in[-] - | B:Committing/upgrading HA path in[-] esc` | 4 | LoseSignal×8, Timeout×8, RecvOther×4 | Timeout→#134×8 | LEGAL-TRANSIENT |
| 131 | `A:Failed/relayed HA path in[-] - | B:Committing/upgrading HA path in[COMPLETE] esc` | 4 | Timeout×8, RecvOther×4, LoseSignal×4 | RecvOther→#67×4, LoseSignal→#130×4 | LEGAL-TRANSIENT |
| 132 | `A:Failed/relayed HA path in[-] - | B:Failed/relayed H- - in[-] esc` | 4 | RecvOther×6, LoseSignal×4, Timeout×4 | - | LEGAL-TERMINAL RelayedOnly |
| 133 | `A:Failed/relayed HA path in[-] - | B:Failed/relayed H- - in[COMPLETE] esc` | 4 | Timeout×4, RecvOther×2 | RecvOther→#132×4, LoseSignal→#132×4 | LEGAL-TRANSIENT |
| 134 | `A:Failed/relayed HA path in[-] - | B:Failed/relayed HA path in[-] esc` | 4 | Timeout×16, RecvOther×8, LoseSignal×8 | - | LEGAL-TERMINAL RelayedOnly |
| 135 | `A:Failed/relayed HA path in[-] - | B:Failed/relayed HA path in[COMPLETE] esc` | 4 | Timeout×8 | RecvOther→#134×4, LoseSignal→#134×4 | LEGAL-TRANSIENT |
| 136 | `A:Failed/relayed HA path in[-] - | B:Punching/upgrading H- - in[-] esc` | 4 | Timeout×4, RecvOther×2, LoseSignal×2 | DeliverAny→#69×4 | LEGAL-TRANSIENT |
| 137 | `A:Failed/relayed HA path in[-] - | B:Punching/upgrading HA - in[COMPLETE] esc` | 4 | Timeout×4, DeliverAny×2 | RecvOther→#69×4, PunchOK→#145×4, LoseSignal→#69×4 | LEGAL-TRANSIENT |
| 138 | `A:Failed/relayed HA path in[-] esc | B:Committing/upgrading HA path in[-] -` | 4 | Timeout×8, LoseSignal×8, RecvOther×4 | Timeout→#140×8 | LEGAL-TRANSIENT |
| 139 | `A:Failed/relayed HA path in[-] esc | B:Committing/upgrading HA path in[COMPLETE] -` | 4 | Timeout×8, RecvOther×4, LoseSignal×4 | RecvOther→#70×4, LoseSignal→#138×4 | LEGAL-TRANSIENT |
| 140 | `A:Failed/relayed HA path in[-] esc | B:Failed/relayed HA path in[-] -` | 4 | Timeout×16, RecvOther×8, LoseSignal×8 | - | LEGAL-TERMINAL RelayedOnly |
| 141 | `A:Failed/relayed HA path in[-] esc | B:Failed/relayed HA path in[COMPLETE] -` | 4 | Timeout×8 | RecvOther→#140×4, LoseSignal→#140×4 | LEGAL-TRANSIENT |
| 142 | `A:Failed/relayed HA path in[-] esc | B:Punching/upgrading H- - in[COMPLETE] esc` | 4 | Timeout×4 | RecvOther→#72×4, LoseSignal→#72×4, DeliverAny→#31×4 | LEGAL-TRANSIENT |
| 143 | `A:Failed/relayed HA path in[-] esc | B:Punching/upgrading HA - in[COMPLETE] -` | 4 | Timeout×4 | RecvOther→#73×4, PunchOK→#148×4, LoseSignal→#73×4, Tick→#31×2 | LEGAL-TRANSIENT |
| 144 | `A:Failed/relayed HA path in[COMPLETE] - | B:Committing/upgrading HA path in[-] esc` | 4 | PunchOK×4, LoseSignal×4 | Timeout→#146×8, RecvOther→#130×4, LoseSignal→#130×4 | LEGAL-TRANSIENT |
| 145 | `A:Failed/relayed HA path in[COMPLETE] - | B:Committing/upgrading HA path in[COMPLETE] esc` | 4 | PunchOK×4 | RecvOther→#131×4, LoseSignal→#131×4, RecvOther→#74×4, LoseSignal→#144×4 | LEGAL-TRANSIENT |
| 146 | `A:Failed/relayed HA path in[COMPLETE] - | B:Failed/relayed HA path in[-] esc` | 4 | Timeout×8 | RecvOther→#134×4, LoseSignal→#134×4 | LEGAL-TRANSIENT |
| 147 | `A:Failed/relayed HA path in[COMPLETE] esc | B:Committing/upgrading HA path in[-] -` | 4 | PunchOK×4, LoseSignal×4 | Timeout→#149×8, RecvOther→#138×4, LoseSignal→#138×4 | LEGAL-TRANSIENT |
| 148 | `A:Failed/relayed HA path in[COMPLETE] esc | B:Committing/upgrading HA path in[COMPLETE] -` | 4 | PunchOK×4 | RecvOther→#139×4, LoseSignal→#139×4, RecvOther→#76×4, LoseSignal→#147×4 | LEGAL-TRANSIENT |
| 149 | `A:Failed/relayed HA path in[COMPLETE] esc | B:Failed/relayed HA path in[-] -` | 4 | Timeout×8 | RecvOther→#140×4, LoseSignal→#140×4 | LEGAL-TRANSIENT |
| 150 | `A:Punching/upgrading H- - in[-] esc | B:Committing/upgrading HA path in[-] -` | 4 | RecvOther×2, LoseSignal×2 | Timeout→#151×4, DeliverAny→#82×4 | LEGAL-TRANSIENT |
| 151 | `A:Punching/upgrading H- - in[-] esc | B:Failed/relayed HA path in[-] -` | 4 | Timeout×4, RecvOther×2, LoseSignal×2 | DeliverAny→#83×4 | LEGAL-TRANSIENT |
| 152 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading HA - in[-] esc` | 4 | DeliverAny×4 | PunchOK→#153×4, DeliverAny→#35×4 | LEGAL-TRANSIENT |
| 153 | `A:Punching/upgrading H- - in[COMPLETE] esc | B:Committing/upgrading HA path in[-] esc` | 4 | PunchOK×4 | RecvOther→#78×4, LoseSignal→#78×4, Timeout→#154×4, DeliverAny→#36×4 | LEGAL-TRANSIENT |
| 154 | `A:Punching/upgrading H- - in[COMPLETE] esc | B:Failed/relayed HA path in[-] esc` | 4 | Timeout×4 | RecvOther→#79×4, LoseSignal→#79×4, DeliverAny→#37×4 | LEGAL-TRANSIENT |
| 155 | `A:Punching/upgrading HA - in[-] - | B:Punching/upgrading HA - in[-] esc` | 4 | DeliverAny×2 | PunchOK→#110×4, PunchOK→#158×4, Tick→#35×2 | LEGAL-TRANSIENT |
| 156 | `A:Punching/upgrading HA - in[-] esc | B:Punching/upgrading H- - in[-] esc` | 4 | DeliverAny×4 | PunchOK→#115×4, DeliverAny→#35×4 | LEGAL-TRANSIENT |
| 157 | `A:Punching/upgrading HA - in[-] esc | B:Punching/upgrading HA - in[-] -` | 4 | DeliverAny×2 | PunchOK→#116×4, PunchOK→#160×4, Tick→#35×2 | LEGAL-TRANSIENT |
| 158 | `A:Punching/upgrading HA - in[COMPLETE] - | B:Committing/upgrading HA path in[-] esc` | 4 | PunchOK×4 | RecvOther→#80×4, PunchOK→#118×4, LoseSignal→#80×4, Timeout→#159×4, Tick→#36×2 | LEGAL-TRANSIENT |
| 159 | `A:Punching/upgrading HA - in[COMPLETE] - | B:Failed/relayed HA path in[-] esc` | 4 | Timeout×4 | RecvOther→#81×4, PunchOK→#120×4, LoseSignal→#81×4, Tick→#37×2 | LEGAL-TRANSIENT |
| 160 | `A:Punching/upgrading HA - in[COMPLETE] esc | B:Committing/upgrading HA path in[-] -` | 4 | PunchOK×4, DeliverAny×2 | RecvOther→#82×4, PunchOK→#122×4, LoseSignal→#82×4, Timeout→#161×4 | LEGAL-TRANSIENT |
| 161 | `A:Punching/upgrading HA - in[COMPLETE] esc | B:Failed/relayed HA path in[-] -` | 4 | Timeout×4, DeliverAny×2 | RecvOther→#83×4, PunchOK→#124×4, LoseSignal→#83×4 | LEGAL-TRANSIENT |
| 162 | `A:AwaitingReady/upgrading -- - in[-] - | B:Punching/upgrading -- - in[-] -` | 3 | LoseSignal×3 | Timeout→#164×3, Tick→#180×1 | LEGAL-TRANSIENT |
| 163 | `A:AwaitingReady/upgrading -- - in[READY] - | B:Punching/upgrading -- - in[-] -` | 3 | RecvOther×3, ProbeAnswer×1 | RecvOther→#84×3, LoseSignal→#162×3, Tick→#181×1 | LEGAL-TRANSIENT |
| 164 | `A:Failed/relayed -- - in[-] - | B:Punching/upgrading -- - in[-] -` | 3 | Timeout×6, RecvOther×3, LoseSignal×3 | Tick→#125×1 | LEGAL-TRANSIENT |
| 165 | `A:Failed/relayed -- - in[ACCEPT,READY] - | B:Punching/upgrading -- - in[-] -` | 3 | ProbeAnswer×1 | RecvOther→#166×3, LoseSignal→#166×3, Tick→#126×1 | LEGAL-TRANSIENT |
| 166 | `A:Failed/relayed -- - in[READY] - | B:Punching/upgrading -- - in[-] -` | 3 | RecvOther×3, LoseSignal×3, ProbeAnswer×1 | RecvOther→#164×3, LoseSignal→#164×3, Tick→#127×1 | LEGAL-TRANSIENT |
| 167 | `A:Failed/relayed -A - in[-] esc | B:Failed/relayed H- - in[-] -` | 3 | RecvOther×3 | - | LEGAL-TERMINAL RelayedOnly |
| 168 | `A:Failed/relayed -A - in[-] esc | B:Punching/upgrading H- - in[-] -` | 3 | LoseSignal×3 | Tick→#89×1 | LEGAL-TRANSIENT |
| 169 | `A:Failed/relayed -A - in[-] esc | B:Punching/upgrading H- - in[REJECT] -` | 3 | PunchFail×2 | RecvOther→#167×3, LoseSignal→#168×3, Tick→#90×1 | LEGAL-TRANSIENT |
| 170 | `A:Failed/relayed H- - in[-] - | B:Failed/relayed -A - in[-] esc` | 3 | RecvOther×3 | - | LEGAL-TERMINAL RelayedOnly |
| 171 | `A:Proposing/upgrading -- - in[-] - | B:Punching/upgrading -- - in[-] -` | 3 | RecvOther×3, LoseSignal×3 | Timeout→#164×3, Tick→#210×1 | LEGAL-TRANSIENT |
| 172 | `A:Proposing/upgrading -- - in[ACCEPT,READY] - | B:Punching/upgrading -- - in[-] -` | 3 | ProbeAnswer×1 | RecvOther→#163×3, LoseSignal→#173×3, Tick→#211×1 | LEGAL-TRANSIENT |
| 173 | `A:Proposing/upgrading -- - in[READY] - | B:Punching/upgrading -- - in[-] -` | 3 | LoseSignal×3, ProbeAnswer×1 | RecvOther→#171×3, LoseSignal→#171×3, Tick→#212×1 | LEGAL-TRANSIENT |
| 174 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading -- - in[-] esc` | 3 | RecvOther×2, Tick×1 | DeliverAny→#235×1 | LEGAL-TRANSIENT |
| 175 | `A:Punching/upgrading -- - in[-] esc | B:Punching/upgrading -- - in[-] -` | 3 | Tick×1 | DeliverAny→#241×1 | LEGAL-TRANSIENT |
| 176 | `A:Punching/upgrading -A - in[-] - | B:Punching/upgrading H- - in[-] esc` | 3 | Tick×1, DeliverAny×1 | DeliverAny→#219×2, Tick→#46×1, PunchFail→#238×1 | LEGAL-TRANSIENT |
| 177 | `A:Punching/upgrading H- - in[-] - | B:Failed/relayed -A - in[-] esc` | 3 | LoseSignal×3 | Tick→#101×1 | LEGAL-TRANSIENT |
| 178 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading -A - in[-] -` | 3 | Tick×1, DeliverAny×1 | DeliverAny→#215×2, Tick→#47×1, PunchFail→#228×1 | LEGAL-TRANSIENT |
| 179 | `A:Punching/upgrading H- - in[REJECT] - | B:Failed/relayed -A - in[-] esc` | 3 | PunchFail×2 | RecvOther→#170×3, LoseSignal→#177×3, Tick→#102×1 | LEGAL-TRANSIENT |
| 180 | `A:AwaitingReady/upgrading -- - in[-] - | B:Punching/upgrading -- - in[-] esc` | 2 | LoseSignal×2, Tick×1 | Timeout→#125×2 | LEGAL-TRANSIENT |
| 181 | `A:AwaitingReady/upgrading -- - in[READY] - | B:Punching/upgrading -- - in[-] esc` | 2 | RecvOther×2, Tick×1 | RecvOther→#174×2, LoseSignal→#180×2 | LEGAL-TRANSIENT |
| 182 | `A:Committing/upgrading HA path in[-] - | B:Failed/relayed H- - in[-] esc` | 2 | LoseSignal×4, RecvOther×2 | Timeout→#132×4 | LEGAL-TRANSIENT |
| 183 | `A:Committing/upgrading HA path in[-] - | B:Failed/relayed H- - in[COMPLETE] esc` | 2 | PunchOK×2, LoseSignal×2 | Timeout→#133×4, RecvOther→#182×2, LoseSignal→#182×2 | LEGAL-TRANSIENT |
| 184 | `A:Committing/upgrading HA path in[-] - | B:Punching/upgrading H- - in[COMPLETE] esc` | 2 | PunchOK×2 | Timeout→#209×2, RecvOther→#109×2, LoseSignal→#109×2, DeliverAny→#110×2 | LEGAL-TRANSIENT |
| 185 | `A:Committing/upgrading HA path in[REJECT] - | B:Failed/relayed H- - in[-] esc` | 2 | RecvOther×2, LoseSignal×2 | RecvOther→#132×2, LoseSignal→#182×2 | LEGAL-TRANSIENT |
| 186 | `A:Committing/upgrading HA path in[REJECT] - | B:Failed/relayed H- - in[COMPLETE] esc` | 2 | PunchOK×2 | RecvOther→#133×2, LoseSignal→#183×2, RecvOther→#185×2, LoseSignal→#185×2 | LEGAL-TRANSIENT |
| 187 | `A:Failed/relayed -- - in[-] - | B:Failed/relayed -- - in[-] esc` | 2 | RecvOther×2, LoseSignal×2 | - | LEGAL-TERMINAL RelayedOnly |
| 188 | `A:Failed/relayed -- - in[ACCEPT,READY,REJECT] - | B:Failed/relayed -- - in[-] esc` | 2 | PunchFail×2 | RecvOther→#189×2, LoseSignal→#189×2 | LEGAL-TRANSIENT |
| 189 | `A:Failed/relayed -- - in[READY,REJECT] - | B:Failed/relayed -- - in[-] esc` | 2 | RecvOther×2, LoseSignal×2, PunchFail×2 | RecvOther→#190×2, LoseSignal→#190×2 | LEGAL-TRANSIENT |
| 190 | `A:Failed/relayed -- - in[REJECT] - | B:Failed/relayed -- - in[-] esc` | 2 | PunchFail×2, RecvOther×2, LoseSignal×2 | RecvOther→#187×2, LoseSignal→#187×2 | LEGAL-TRANSIENT |
| 191 | `A:Failed/relayed -A - in[-] - | B:Failed/relayed H- - in[-] esc` | 2 | RecvOther×1 | - | LEGAL-TERMINAL RelayedOnly |
| 192 | `A:Failed/relayed -A - in[-] esc | B:Failed/relayed H- - in[REJECT] esc` | 2 | RecvOther×2, LoseSignal×2 | RecvOther→#40×2, LoseSignal→#40×2 | LEGAL-TRANSIENT |
| 193 | `A:Failed/relayed -A - in[REJECT] esc | B:Failed/relayed H- - in[-] esc` | 2 | PunchFail×2, RecvOther×2, LoseSignal×2 | RecvOther→#40×2, LoseSignal→#40×2 | LEGAL-TRANSIENT |
| 194 | `A:Failed/relayed -A - in[REJECT] esc | B:Failed/relayed H- - in[REJECT] esc` | 2 | PunchFail×2 | RecvOther→#192×2, LoseSignal→#192×2, RecvOther→#193×2, LoseSignal→#193×2 | LEGAL-TRANSIENT |
| 195 | `A:Failed/relayed H- - in[-] esc | B:Committing/upgrading HA path in[-] -` | 2 | LoseSignal×4, RecvOther×2 | Timeout→#128×4 | LEGAL-TRANSIENT |
| 196 | `A:Failed/relayed H- - in[-] esc | B:Committing/upgrading HA path in[REJECT] -` | 2 | RecvOther×2, LoseSignal×2 | RecvOther→#128×2, LoseSignal→#195×2 | LEGAL-TRANSIENT |
| 197 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed -A - in[-] -` | 2 | RecvOther×1 | - | LEGAL-TERMINAL RelayedOnly |
| 198 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed -A - in[REJECT] esc` | 2 | PunchFail×2, RecvOther×2, LoseSignal×2 | RecvOther→#41×2, LoseSignal→#41×2 | LEGAL-TRANSIENT |
| 199 | `A:Failed/relayed H- - in[-] esc | B:Failed/relayed HA - in[-] -` | 2 | RecvOther×2 | - | LEGAL-TERMINAL RelayedOnly |
| 200 | `A:Failed/relayed H- - in[-] esc | B:Punching/upgrading -A - in[-] esc` | 2 | LoseSignal×2 | DeliverAny→#94×2 | LEGAL-TRANSIENT |
| 201 | `A:Failed/relayed H- - in[-] esc | B:Punching/upgrading -A - in[REJECT] esc` | 2 | PunchFail×2 | RecvOther→#41×2, LoseSignal→#200×2, DeliverAny→#95×2 | LEGAL-TRANSIENT |
| 202 | `A:Failed/relayed H- - in[-] esc | B:Punching/upgrading HA - in[-] -` | 2 | LoseSignal×2, DeliverAny×1 | PunchOK→#204×2, Tick→#94×1 | LEGAL-TRANSIENT |
| 203 | `A:Failed/relayed H- - in[-] esc | B:Punching/upgrading HA - in[REJECT] -` | 2 | DeliverAny×1 | RecvOther→#199×2, PunchOK→#205×2, LoseSignal→#202×2, Tick→#95×1 | LEGAL-TRANSIENT |
| 204 | `A:Failed/relayed H- - in[COMPLETE] esc | B:Committing/upgrading HA path in[-] -` | 2 | PunchOK×2, LoseSignal×2 | Timeout→#129×4, RecvOther→#195×2, LoseSignal→#195×2 | LEGAL-TRANSIENT |
| 205 | `A:Failed/relayed H- - in[COMPLETE] esc | B:Committing/upgrading HA path in[REJECT] -` | 2 | PunchOK×2 | RecvOther→#196×2, LoseSignal→#196×2, RecvOther→#129×2, LoseSignal→#204×2 | LEGAL-TRANSIENT |
| 206 | `A:Failed/relayed H- - in[REJECT] esc | B:Failed/relayed -A - in[-] esc` | 2 | RecvOther×2, LoseSignal×2 | RecvOther→#41×2, LoseSignal→#41×2 | LEGAL-TRANSIENT |
| 207 | `A:Failed/relayed H- - in[REJECT] esc | B:Failed/relayed -A - in[REJECT] esc` | 2 | PunchFail×2 | RecvOther→#198×2, LoseSignal→#198×2, RecvOther→#206×2, LoseSignal→#206×2 | LEGAL-TRANSIENT |
| 208 | `A:Failed/relayed HA - in[-] - | B:Failed/relayed H- - in[-] esc` | 2 | RecvOther×2 | - | LEGAL-TERMINAL RelayedOnly |
| 209 | `A:Failed/relayed HA path in[-] - | B:Punching/upgrading H- - in[COMPLETE] esc` | 2 | Timeout×2 | RecvOther→#136×2, LoseSignal→#136×2, DeliverAny→#137×2 | LEGAL-TRANSIENT |
| 210 | `A:Proposing/upgrading -- - in[-] - | B:Punching/upgrading -- - in[-] esc` | 2 | RecvOther×2, LoseSignal×2, Tick×1 | Timeout→#125×2 | LEGAL-TRANSIENT |
| 211 | `A:Proposing/upgrading -- - in[ACCEPT,READY] - | B:Punching/upgrading -- - in[-] esc` | 2 | Tick×1 | RecvOther→#181×2, LoseSignal→#212×2 | LEGAL-TRANSIENT |
| 212 | `A:Proposing/upgrading -- - in[READY] - | B:Punching/upgrading -- - in[-] esc` | 2 | LoseSignal×2, Tick×1 | RecvOther→#210×2, LoseSignal→#210×2 | LEGAL-TRANSIENT |
| 213 | `A:Punching/upgrading -A - in[-] esc | B:Failed/relayed H- - in[-] esc` | 2 | LoseSignal×2 | DeliverAny→#103×2 | LEGAL-TRANSIENT |
| 214 | `A:Punching/upgrading -A - in[REJECT] esc | B:Failed/relayed H- - in[-] esc` | 2 | PunchFail×2 | RecvOther→#40×2, LoseSignal→#213×2, DeliverAny→#104×2 | LEGAL-TRANSIENT |
| 215 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading HA - in[-] -` | 2 | DeliverAny×2 | PunchOK→#216×2, DeliverAny→#157×2 | LEGAL-TRANSIENT |
| 216 | `A:Punching/upgrading H- - in[COMPLETE] esc | B:Committing/upgrading HA path in[-] -` | 2 | PunchOK×2 | RecvOther→#150×2, LoseSignal→#150×2, Timeout→#217×2, DeliverAny→#160×2 | LEGAL-TRANSIENT |
| 217 | `A:Punching/upgrading H- - in[COMPLETE] esc | B:Failed/relayed HA path in[-] -` | 2 | Timeout×2 | RecvOther→#151×2, LoseSignal→#151×2, DeliverAny→#161×2 | LEGAL-TRANSIENT |
| 218 | `A:Punching/upgrading HA - in[-] - | B:Failed/relayed H- - in[-] esc` | 2 | LoseSignal×2, DeliverAny×1 | PunchOK→#183×2, Tick→#103×1 | LEGAL-TRANSIENT |
| 219 | `A:Punching/upgrading HA - in[-] - | B:Punching/upgrading H- - in[-] esc` | 2 | DeliverAny×2 | PunchOK→#184×2, DeliverAny→#155×2 | LEGAL-TRANSIENT |
| 220 | `A:Punching/upgrading HA - in[REJECT] - | B:Failed/relayed H- - in[-] esc` | 2 | DeliverAny×1 | RecvOther→#208×2, PunchOK→#186×2, LoseSignal→#218×2, Tick→#104×1 | LEGAL-TRANSIENT |
| 221 | `A:AwaitingReady/upgrading -- - in[-] - | B:Discovering/upgrading -- - in[-] -` | 1 | RecvOther×1 | Timeout→#223×1, ProbeAnswer→#163×1 | LEGAL-TRANSIENT |
| 222 | `A:Discovering/upgrading -- - in[-] - | B:Idle/relayed -- - in[-] -` | 1 | Propose×1 | ProbeAnswer→#232×1 | LEGAL-TRANSIENT |
| 223 | `A:Failed/relayed -- - in[-] - | B:Discovering/upgrading -- - in[-] -` | 1 | Timeout×2, RecvOther×1, LoseSignal×1 | ProbeAnswer→#166×1 | LEGAL-TRANSIENT |
| 224 | `A:Failed/relayed -- - in[-] - | B:Idle/relayed -- - in[-] -` | 1 | Timeout×2, LoseSignal×1 | - | LEGAL-TERMINAL RelayedOnly |
| 225 | `A:Failed/relayed -- - in[-] - | B:Idle/relayed -- - in[REQUEST] -` | 1 | Timeout×1 | RecvRequest→#226×1, LoseSignal→#224×1 | LEGAL-TRANSIENT |
| 226 | `A:Failed/relayed -- - in[ACCEPT] - | B:Discovering/upgrading -- - in[-] -` | 1 | RecvRequest×1 | RecvOther→#223×1, LoseSignal→#223×1, ProbeAnswer→#165×1 | LEGAL-TRANSIENT |
| 227 | `A:Failed/relayed H- - in[-] esc | B:Punching/upgrading -A - in[-] -` | 1 | LoseSignal×1 | DeliverAny→#202×1 | LEGAL-TRANSIENT |
| 228 | `A:Failed/relayed H- - in[-] esc | B:Punching/upgrading -A - in[REJECT] -` | 1 | PunchFail×1 | RecvOther→#197×1, LoseSignal→#227×1, DeliverAny→#203×1 | LEGAL-TRANSIENT |
| 229 | `A:Idle/relayed -- - in[-] - | B:Idle/relayed -- - in[-] -` | 1 | - | Propose→#222×1 | LEGAL-TRANSIENT |
| 230 | `A:Proposing/upgrading -- - in[-] - | B:Discovering/upgrading -- - in[-] -` | 1 | LoseSignal×1 | Timeout→#223×1, ProbeAnswer→#173×1 | LEGAL-TRANSIENT |
| 231 | `A:Proposing/upgrading -- - in[-] - | B:Idle/relayed -- - in[-] -` | 1 | LoseSignal×1 | Timeout→#224×2 | LEGAL-TRANSIENT |
| 232 | `A:Proposing/upgrading -- - in[-] - | B:Idle/relayed -- - in[REQUEST] -` | 1 | ProbeAnswer×1 | Timeout→#225×1, RecvRequest→#233×1, LoseSignal→#231×1 | LEGAL-TRANSIENT |
| 233 | `A:Proposing/upgrading -- - in[ACCEPT] - | B:Discovering/upgrading -- - in[-] -` | 1 | RecvRequest×1 | RecvOther→#221×1, LoseSignal→#230×1, ProbeAnswer→#172×1 | LEGAL-TRANSIENT |
| 234 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading H- - in[-] -` | 1 | DeliverAny×1 | DeliverAny→#237×1 | LEGAL-TRANSIENT |
| 235 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading H- - in[-] esc` | 1 | DeliverAny×1 | DeliverAny→#176×1 | LEGAL-TRANSIENT |
| 236 | `A:Punching/upgrading -A - in[-] - | B:Failed/relayed H- - in[-] esc` | 1 | LoseSignal×1 | DeliverAny→#218×1 | LEGAL-TRANSIENT |
| 237 | `A:Punching/upgrading -A - in[-] - | B:Punching/upgrading H- - in[-] -` | 1 | DeliverAny×1 | Tick→#99×1, Tick→#176×1 | LEGAL-TRANSIENT |
| 238 | `A:Punching/upgrading -A - in[REJECT] - | B:Failed/relayed H- - in[-] esc` | 1 | PunchFail×1 | RecvOther→#191×1, LoseSignal→#236×1, DeliverAny→#220×1 | LEGAL-TRANSIENT |
| 239 | `A:Punching/upgrading H- - in[-] - | B:Punching/upgrading -- - in[-] -` | 1 | DeliverAny×1 | DeliverAny→#240×1 | LEGAL-TRANSIENT |
| 240 | `A:Punching/upgrading H- - in[-] - | B:Punching/upgrading -A - in[-] -` | 1 | DeliverAny×1 | Tick→#178×1, Tick→#100×1 | LEGAL-TRANSIENT |
| 241 | `A:Punching/upgrading H- - in[-] esc | B:Punching/upgrading -- - in[-] -` | 1 | DeliverAny×1 | DeliverAny→#178×1 | LEGAL-TRANSIENT |

Classes with no path to a terminal state, or a rejected terminal: 0.
