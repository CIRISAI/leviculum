### State classes: facilitator-down

3 states, 2 transitions, 3 classes. Class: per side `phase/linkPath heard-acked path in[signals in flight] flags`.

| # | class (A \| B) | states | in (by action) | out (action -> class) | verdict |
|---|---|---|---|---|---|
| 1 | `A:Discovering/upgrading -- - in[-] - | B:Idle/relayed -- - in[-] -` | 1 | Propose×1 | ProbeFail→#2×1 | LEGAL-TRANSIENT |
| 2 | `A:Failed/relayed -- - in[-] - | B:Idle/relayed -- - in[-] -` | 1 | ProbeFail×1 | - | LEGAL-TERMINAL RelayedOnly |
| 3 | `A:Idle/relayed -- - in[-] - | B:Idle/relayed -- - in[-] -` | 1 | - | Propose→#1×1 | LEGAL-TRANSIENT |

Classes with no path to a terminal state, or a rejected terminal: 0.
