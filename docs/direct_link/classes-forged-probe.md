### State classes: forged-probe

106 states, 156 transitions, 19 classes. Class: per side `phase/linkPath heard-acked path in[signals in flight] flags`.

| # | class (A \| B) | states | in (by action) | out (action -> class) | verdict |
|---|---|---|---|---|---|
| 1 | `A:Punching/upgrading -- - in[-] esc | B:Punching/upgrading -- - in[-] esc` | 25 | Tick×4 | PunchFail→#3×6, PunchFail→#5×6 | LEGAL-TRANSIENT |
| 2 | `A:Failed/relayed -- - in[-] esc | B:Failed/relayed -- - in[-] esc` | 19 | RecvOther×22 | - | LEGAL-TERMINAL RelayedOnly |
| 3 | `A:Failed/relayed -- - in[-] esc | B:Punching/upgrading -- - in[REJECT] esc` | 8 | PunchFail×6 | RecvOther→#2×8, PunchFail→#11×2 | LEGAL-TRANSIENT |
| 4 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading -- - in[-] -` | 8 | RecvOther×3 | Tick→#7×2, Tick→#6×2 | LEGAL-TRANSIENT |
| 5 | `A:Punching/upgrading -- - in[REJECT] esc | B:Failed/relayed -- - in[-] esc` | 8 | PunchFail×6 | RecvOther→#2×8, PunchFail→#11×2 | LEGAL-TRANSIENT |
| 6 | `A:Punching/upgrading -- - in[-] - | B:Punching/upgrading -- - in[-] esc` | 7 | RecvOther×2, Tick×2 | Tick→#1×2 | LEGAL-TRANSIENT |
| 7 | `A:Punching/upgrading -- - in[-] esc | B:Punching/upgrading -- - in[-] -` | 7 | Tick×2 | Tick→#1×2 | LEGAL-TRANSIENT |
| 8 | `A:AwaitingReady/upgrading -- - in[READY] - | B:Punching/upgrading -- - in[-] -` | 3 | RecvOther×3, ProbeAnswer×1 | RecvOther→#4×3, Tick→#13×1 | LEGAL-TRANSIENT |
| 9 | `A:Failed/relayed -- - in[-] esc | B:Failed/relayed -- - in[REJECT] esc` | 3 | RecvOther×3 | RecvOther→#2×3 | LEGAL-TRANSIENT |
| 10 | `A:Failed/relayed -- - in[REJECT] esc | B:Failed/relayed -- - in[-] esc` | 3 | RecvOther×3 | RecvOther→#2×3 | LEGAL-TRANSIENT |
| 11 | `A:Failed/relayed -- - in[REJECT] esc | B:Failed/relayed -- - in[REJECT] esc` | 3 | PunchFail×4 | RecvOther→#9×3, RecvOther→#10×3 | LEGAL-TRANSIENT |
| 12 | `A:Proposing/upgrading -- - in[ACCEPT,READY] - | B:Punching/upgrading -- - in[-] -` | 3 | ProbeAnswer×1 | RecvOther→#8×3, Tick→#14×1 | LEGAL-TRANSIENT |
| 13 | `A:AwaitingReady/upgrading -- - in[READY] - | B:Punching/upgrading -- - in[-] esc` | 2 | RecvOther×2, Tick×1 | RecvOther→#6×2 | LEGAL-TRANSIENT |
| 14 | `A:Proposing/upgrading -- - in[ACCEPT,READY] - | B:Punching/upgrading -- - in[-] esc` | 2 | Tick×1 | RecvOther→#13×2 | LEGAL-TRANSIENT |
| 15 | `A:AwaitingReady/upgrading -- - in[-] - | B:Discovering/upgrading -- - in[-] -` | 1 | RecvOther×1 | ProbeAnswer→#8×1 | LEGAL-TRANSIENT |
| 16 | `A:Discovering/upgrading -- - in[-] - | B:Idle/relayed -- - in[-] -` | 1 | Propose×1 | ProbeAnswer→#18×1 | LEGAL-TRANSIENT |
| 17 | `A:Idle/relayed -- - in[-] - | B:Idle/relayed -- - in[-] -` | 1 | - | Propose→#16×1 | LEGAL-TRANSIENT |
| 18 | `A:Proposing/upgrading -- - in[-] - | B:Idle/relayed -- - in[REQUEST] -` | 1 | ProbeAnswer×1 | RecvRequest→#19×1 | LEGAL-TRANSIENT |
| 19 | `A:Proposing/upgrading -- - in[ACCEPT] - | B:Discovering/upgrading -- - in[-] -` | 1 | RecvRequest×1 | RecvOther→#15×1, ProbeAnswer→#12×1 | LEGAL-TRANSIENT |

Classes with no path to a terminal state, or a rejected terminal: 0.
