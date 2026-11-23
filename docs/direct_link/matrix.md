## NAT pairs (outcome class over every reachable terminal state)

| A \ B | EIM-EIF | EIM-ADF | EIM-APDF | EDMseq-EIF | EDMseq-ADF | EDMseq-APDF | EDMrand-EIF | EDMrand-ADF | EDMrand-APDF |
|---|---|---|---|---|---|---|---|---|---|
| EIM-EIF | direct | direct | direct | direct | direct | direct | direct | direct | direct |
| EIM-ADF | direct | direct | direct | direct | direct | direct | direct | direct | direct |
| EIM-APDF | direct | direct | direct | direct | direct | direct | direct or relayed | direct or relayed | direct or relayed |
| EDMseq-APDF | direct | direct | direct | direct | relayed only | relayed only | direct | relayed only | relayed only |
| EDMrand-APDF | direct | direct | direct or relayed | direct | relayed only | relayed only | direct [capped] | relayed only | relayed only |

## Every scenario

| scenario | outcomes reached (terminal states) | invariants violated | liveness | run |
|---|---|---|---|---|
| pair-EIM-EIF--EIM-EIF | DirectPathUp ×288 | none | ok | complete, 1249 states |
| pair-EIM-EIF--EIM-ADF | DirectPathUp ×288 | none | ok | complete, 1258 states |
| pair-EIM-EIF--EIM-APDF | DirectPathUp ×288 | none | ok | complete, 1258 states |
| pair-EIM-EIF--EDMseq-EIF | DirectPathUp ×915 | none | ok | complete, 3953 states |
| pair-EIM-EIF--EDMseq-ADF | DirectPathUp ×441 | none | ok | complete, 1929 states |
| pair-EIM-EIF--EDMseq-APDF | DirectPathUp ×441 | none | ok | complete, 1929 states |
| pair-EIM-EIF--EDMrand-EIF | DirectPathUp ×72792 | none | ok | complete, 311501 states |
| pair-EIM-EIF--EDMrand-ADF | DirectPathUp ×58896 | none | ok | complete, 255217 states |
| pair-EIM-EIF--EDMrand-APDF | DirectPathUp ×58896 | none | ok | complete, 255217 states |
| pair-EIM-ADF--EIM-EIF | DirectPathUp ×288 | none | ok | complete, 1243 states |
| pair-EIM-ADF--EIM-ADF | DirectPathUp ×273 | none | ok | complete, 1187 states |
| pair-EIM-ADF--EIM-APDF | DirectPathUp ×273 | none | ok | complete, 1187 states |
| pair-EIM-ADF--EDMseq-EIF | DirectPathUp ×717 | none | ok | complete, 3122 states |
| pair-EIM-ADF--EDMseq-ADF | DirectPathUp ×378 | none | ok | complete, 1654 states |
| pair-EIM-ADF--EDMseq-APDF | DirectPathUp ×378 | none | ok | complete, 1654 states |
| pair-EIM-ADF--EDMrand-EIF | DirectPathUp ×24624 | none | ok | complete, 108389 states |
| pair-EIM-ADF--EDMrand-ADF | DirectPathUp ×34272 | none | ok | complete, 150697 states |
| pair-EIM-ADF--EDMrand-APDF | DirectPathUp ×34272 | none | ok | complete, 150697 states |
| pair-EIM-APDF--EIM-EIF | DirectPathUp ×288 | none | ok | complete, 1243 states |
| pair-EIM-APDF--EIM-ADF | DirectPathUp ×273 | none | ok | complete, 1187 states |
| pair-EIM-APDF--EIM-APDF | DirectPathUp ×273 | none | ok | complete, 1187 states |
| pair-EIM-APDF--EDMseq-EIF | DirectPathUp ×645 | none | ok | complete, 2859 states |
| pair-EIM-APDF--EDMseq-ADF | DirectPathUp ×261 | none | ok | complete, 1180 states |
| pair-EIM-APDF--EDMseq-APDF | DirectPathUp ×261 | none | ok | complete, 1180 states |
| pair-EIM-APDF--EDMrand-EIF | DirectPathUp ×17136, RelayedOnly(A:PunchFailed,B:PunchFailed) ×1728, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×2712, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×912 | none | ok | complete, 96005 states |
| pair-EIM-APDF--EDMrand-ADF | DirectPathUp ×19152, RelayedOnly(A:PunchFailed,B:PunchFailed) ×1344, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×1608, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×1320 | none | ok | complete, 100249 states |
| pair-EIM-APDF--EDMrand-APDF | DirectPathUp ×19152, RelayedOnly(A:PunchFailed,B:PunchFailed) ×1344, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×1608, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×1320 | none | ok | complete, 100249 states |
| pair-EDMseq-APDF--EIM-EIF | DirectPathUp ×441 | none | ok | complete, 1898 states |
| pair-EDMseq-APDF--EIM-ADF | DirectPathUp ×378 | none | ok | complete, 1654 states |
| pair-EDMseq-APDF--EIM-APDF | DirectPathUp ×261 | none | ok | complete, 1180 states |
| pair-EDMseq-APDF--EDMseq-EIF | DirectPathUp ×759 | none | ok | complete, 3243 states |
| pair-EDMseq-APDF--EDMseq-ADF | RelayedOnly(A:PunchFailed,B:PunchFailed) ×28, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×11, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×11 | none | ok | complete, 166 states |
| pair-EDMseq-APDF--EDMseq-APDF | RelayedOnly(A:PunchFailed,B:PunchFailed) ×28, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×11, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×11 | none | ok | complete, 166 states |
| pair-EDMseq-APDF--EDMrand-EIF | DirectPathUp ×20988 | none | ok | complete, 89345 states |
| pair-EDMseq-APDF--EDMrand-ADF | RelayedOnly(A:PunchFailed,B:PunchFailed) ×5184, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×1584, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×2448 | none | ok | complete, 23617 states |
| pair-EDMseq-APDF--EDMrand-APDF | RelayedOnly(A:PunchFailed,B:PunchFailed) ×5184, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×1584, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×2448 | none | ok | complete, 23617 states |
| pair-EDMrand-APDF--EIM-EIF | DirectPathUp ×39312 | none | ok | complete, 169538 states |
| pair-EDMrand-APDF--EIM-ADF | DirectPathUp ×34272 | none | ok | complete, 150530 states |
| pair-EDMrand-APDF--EIM-APDF | DirectPathUp ×19152, RelayedOnly(A:PunchFailed,B:PunchFailed) ×1344, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×1320, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×1608 | none | ok | complete, 100082 states |
| pair-EDMrand-APDF--EDMseq-EIF | DirectPathUp ×70416 | none | ok | complete, 301154 states |
| pair-EDMrand-APDF--EDMseq-ADF | RelayedOnly(A:PunchFailed,B:PunchFailed) ×5184, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×2448, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×1584 | none | ok | complete, 23450 states |
| pair-EDMrand-APDF--EDMseq-APDF | RelayedOnly(A:PunchFailed,B:PunchFailed) ×5184, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×2448, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×1584 | none | ok | complete, 23450 states |
| pair-EDMrand-APDF--EDMrand-EIF | DirectPathUp ×243918 | none | ok | PARTIAL (capped), 1340191 states |
| pair-EDMrand-APDF--EDMrand-ADF | RelayedOnly(A:PunchFailed,B:PunchFailed) ×580608, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×228096, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×228096 | none | ok | complete, 2462846 states |
| pair-EDMrand-APDF--EDMrand-APDF | RelayedOnly(A:PunchFailed,B:PunchFailed) ×580608, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×228096, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×228096 | none | ok | complete, 2462846 states |
| observers-1-hard | DirectPathUp ×19152, RelayedOnly(A:PunchFailed,B:PunchFailed) ×1344, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×1608, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×1320 | none | ok | complete, 99925 states |
| observers-1-seq | RelayedOnly(A:PunchFailed,B:PunchFailed) ×12, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×5, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×5 | none | ok | complete, 86 states |
| observers-2-hard | DirectPathUp ×19152, RelayedOnly(A:PunchFailed,B:PunchFailed) ×1344, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×1608, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×1320 | none | ok | complete, 100249 states |
| observers-2-seq | DirectPathUp ×261 | none | ok | complete, 1180 states |
| observers-3-hard | DirectPathUp ×19152, RelayedOnly(A:PunchFailed,B:PunchFailed) ×1344, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×1608, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×1320 | none | ok | complete, 100249 states |
| observers-3-seq | DirectPathUp ×261 | none | ok | complete, 1180 states |
| observers-4-hard | DirectPathUp ×19152, RelayedOnly(A:PunchFailed,B:PunchFailed) ×1344, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×1608, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×1320 | none | ok | complete, 100249 states |
| observers-4-seq | DirectPathUp ×261 | none | ok | complete, 1180 states |
| hard-spray-hit | DirectPathUp ×11448 | none | ok | complete, 55441 states |
| hard-spray-miss | DirectPathUp ×7704, RelayedOnly(A:PunchFailed,B:PunchFailed) ×384, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×984, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×240 | none | ok | complete, 39481 states |
| hard-budget-refused | RelayedOnly(A:PunchFailed,B:PunchFailed) ×144, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×60, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×60 | none | ok | complete, 937 states |
| hard-stranger | DirectPathUp ×7758, RelayedOnly(A:PunchFailed,B:PunchFailed) ×1008, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×1206, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×990 | none | ok | complete, 43976 states |
| hard-latency | DirectPathUp ×69024, RelayedOnly(A:PunchFailed,B:PunchFailed) ×34, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×13891, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×6322 | none | ok | PARTIAL (capped), 870854 states |
| hard-pooling-arbitrary | RelayedOnly(A:PunchFailed,B:PunchFailed) ×1440, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×504, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×648 | none | ok | complete, 8281 states |
| seq-pooling-arbitrary | RelayedOnly(A:PunchFailed,B:PunchFailed) ×20, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×9, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×7 | none | ok | complete, 126 states |
| hard-accept-all | DirectPathUp ×3888, RelayedOnly(A:PunchFailed,B:PunchFailed) ×528, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×900, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×228 | none | ok | complete, 21877 states |
| gate-accept-all-host-allow | DirectPathUp ×19152, RelayedOnly(A:PunchFailed,B:PunchFailed) ×1344, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×1608, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×1320 | none | ok | complete, 100249 states |
| gate-host-deny-b | DirectPathUp ×3888, RelayedOnly(A:PunchFailed,B:PunchFailed) ×528, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×900, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×228 | none | ok | complete, 21877 states |
| gate-host-deny-a | RelayedOnly(A:PunchFailed,B:PunchFailed) ×480, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×168, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×216 | none | ok | complete, 3001 states |
| gate-host-deny-both | RelayedOnly(A:PunchFailed,B:PunchFailed) ×144, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×60, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×60 | none | ok | complete, 937 states |
| gate-identified-only | DirectPathUp ×19152, RelayedOnly(A:PunchFailed,B:PunchFailed) ×1344, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×1608, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×1320 | none | ok | complete, 100249 states |
| gate-identified-only-anonymous | DirectPathUp ×3888, RelayedOnly(A:PunchFailed,B:PunchFailed) ×528, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×900, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×228 | none | ok | complete, 21877 states |
| gate-b-initiated-link | RelayedOnly(A:PunchFailed,B:PunchFailed) ×480, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×168, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×216 | none | ok | complete, 3001 states |
| gate-b-initiated-link-a-allowed | DirectPathUp ×19152, RelayedOnly(A:PunchFailed,B:PunchFailed) ×1344, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×1608, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×1320 | none | ok | complete, 100249 states |
| gate-random-proposer-b-initiated | DirectPathUp ×3888, RelayedOnly(A:PunchFailed,B:PunchFailed) ×528, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×228, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×900 | none | ok | complete, 21854 states |
| sockets-two-observer-K2 | DirectPathUp ×36360 | none | ok | complete, 172561 states |
| sockets-fallback-KF1 | DirectPathUp ×19152, RelayedOnly(A:PunchFailed,B:PunchFailed) ×1344, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×1608, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×1320 | none | ok | complete, 99925 states |
| sockets-fallback-KF2 | DirectPathUp ×36360 | none | ok | complete, 171853 states |
| forged-probe | RelayedOnly(A:PunchFailed,B:PunchFailed) ×12, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×5, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×5 | none | ok | complete, 86 states |
| forged-probe-hard | RelayedOnly(A:PunchFailed,B:PunchFailed) ×1440, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×504, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×648 | none | ok | complete, 8281 states |
| easy-glare-a-wins | DirectPathUp ×819, RelayedOnly(A:Timeout,B:Glare) ×1, RelayedOnly(A:Timeout,B:PunchFailed) ×8 | none | ok | complete, 4256 states |
| easy-glare-b-wins | DirectPathUp ×819, RelayedOnly(A:Glare,B:Timeout) ×1, RelayedOnly(A:PunchFailed,B:Timeout) ×8 | none | ok | complete, 4256 states |
| easy-policy-reject | RelayedOnly(A:Rejected(POLICY),B:-) ×1 | none | ok | complete, 5 states |
| easy-no-facilitator-answer | RelayedOnly(A:ProbeFailed,B:-) ×1 | none | ok | complete, 3 states |
| easy-relay-lossy | DirectPathUp ×273, RelayedOnly(A:Timeout,B:-) ×2, RelayedOnly(A:Timeout,B:PunchFailed) ×8, RelayedOnly(A:Timeout,B:Timeout) ×273 | NoLoneMove | ok | complete, 5087 states |
| easy-relay-dup | DirectPathUp ×273 | none | ok | complete, 1187 states |
| easy-net-lossy | DirectPathUp ×273, RelayedOnly(A:PunchFailed,B:PunchFailed) ×96, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×106, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×106 | none | ok | complete, 2388 states |
| easy-teardown | DirectPathUp ×273, LinkClosed ×706 | none | ok | complete, 1894 states |
| easy-keepalive-lost | DirectPathUp ×1092 | none | ok | complete, 2390 states |
| easy-own-facilitators-differ | DirectPathUp ×273 | none | ok | complete, 1187 states |
| samenat-hairpin | DirectPathUp ×1674 | SamePath | ok | complete, 9803 states |
| samenat-no-hairpin | DirectPathUp ×594 | none | ok | complete, 2568 states |
| samenat-hard-no-hairpin | DirectPathUp ×28476 | none | ok | complete, 121709 states |
| skew-1 | DirectPathUp ×30240, RelayedOnly(A:PunchFailed,B:PunchFailed) ×8160, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×9912, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×8544 | none | ok | complete, 207729 states |
| skew-1-seq | DirectPathUp ×384, RelayedOnly(A:PunchFailed,B:PunchFailed) ×16, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×54, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×14 | none | ok | complete, 2031 states |
| mac-EDMseq-APDF--EDMseq-ADF | DirectPathUp ×813 | none | ok | complete, 3496 states |
| mac-EDMseq-APDF--EDMseq-APDF | RelayedOnly(A:PunchFailed,B:PunchFailed) ×28, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×11, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×11 | none | ok | complete, 166 states |
| mac-EDMseq-APDF--EDMrand-ADF | DirectPathUp ×11342 | SamePath | ok | complete, 83830 states |
| mac-EDMseq-APDF--EDMrand-APDF | RelayedOnly(A:PunchFailed,B:PunchFailed) ×5184, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×1584, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×2448 | none | ok | complete, 23617 states |
| mac-EDMrand-APDF--EDMseq-ADF | DirectPathUp ×16560 | none | ok | complete, 71594 states |
| mac-EDMrand-APDF--EDMseq-APDF | RelayedOnly(A:PunchFailed,B:PunchFailed) ×5184, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×2448, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×1584 | none | ok | complete, 23450 states |
| mac-EDMrand-APDF--EDMrand-ADF | DirectPathUp ×22086 | none | ok | PARTIAL (capped), 667605 states |
| mac-EDMrand-APDF--EDMrand-APDF | DirectPathUp ×13578 | none | ok | PARTIAL (capped), 721725 states |
| mac-hard-stranger | DirectPathUp ×12726 | none | ok | complete, 56408 states |
| residual5-edm-eif | DirectPathUp ×17136, RelayedOnly(A:PunchFailed,B:PunchFailed) ×1728, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×2712, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×912 | none | ok | complete, 96005 states |
| residual5-edm-eif-fallback | DirectPathUp ×6480, RelayedOnly(A:PunchFailed,B:PunchFailed) ×192, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×1884, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×72 | none | ok | complete, 36377 states |
| plant-hijack | - | StrangerKnowledge | VIOLATED | complete, 1614 states |
| plant-named-facilitator | - | FacilitatorKnowledge | ok | complete, 1187 states |
