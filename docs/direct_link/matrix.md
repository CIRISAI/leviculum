## How to read this

TLC explores every behaviour the model allows, so each row says which outcomes are REACHABLE for that pair and scenario, never how likely they are. "direct or relayed" means both can happen, not that they are equally common: the live runs measure likelihood. In particular a pair where BOTH sides map at random (EDMrand x EDMrand) needs the spray to hit a birthday socket on each side at once; the odds of that are the single-side odds squared over a far larger space, and in practice such pairs are negligible (two random carrier NATs facing each other), so a direct outcome there is reachable but not to be expected.

`Observers` counts the observers that ANSWER a side's probe: a second observer configured but silent (or skipped on a late probe) is `Observers = 1`, which is what the code does since ciris.5 (it used to end the own-evidence fallback once one merely resolved).

## NAT pairs (outcome class over every reachable terminal state)

| A \ B | EIM-EIF | EIM-ADF | EIM-APDF | EDMseq-EIF | EDMseq-ADF | EDMseq-APDF | EDMrand-EIF | EDMrand-ADF | EDMrand-APDF |
|---|---|---|---|---|---|---|---|---|---|
| EIM-EIF | direct | direct | direct | direct | direct | direct | direct | direct | direct |
| EIM-ADF | direct | direct | direct | direct | direct | direct | direct | direct | direct |
| EIM-APDF | direct | direct | direct | direct | direct | direct | direct or relayed | direct or relayed | direct or relayed |
| EDMseq-APDF | direct | direct | direct | direct | relayed only | relayed only | direct | relayed only | relayed only |
| EDMrand-APDF | direct | direct | direct or relayed | direct | relayed only | relayed only | direct [capped] | relayed only [capped] | relayed only [capped] |

## Every scenario

| scenario | outcomes reached (terminal states) | invariants violated | liveness | run |
|---|---|---|---|---|
| pair-EIM-EIF--EIM-EIF | DirectPathUp ×28 | none | ok | complete, 164 states |
| pair-EIM-EIF--EIM-ADF | DirectPathUp ×30 | none | ok | complete, 184 states |
| pair-EIM-EIF--EIM-APDF | DirectPathUp ×30 | none | ok | complete, 184 states |
| pair-EIM-EIF--EDMseq-EIF | DirectPathUp ×58 | none | ok | complete, 350 states |
| pair-EIM-EIF--EDMseq-ADF | DirectPathUp ×46 | none | ok | complete, 288 states |
| pair-EIM-EIF--EDMseq-APDF | DirectPathUp ×46 | none | ok | complete, 288 states |
| pair-EIM-EIF--EDMrand-EIF | DirectPathUp ×8928 | none | ok | complete, 44177 states |
| pair-EIM-EIF--EDMrand-ADF | DirectPathUp ×8160 | none | ok | complete, 40225 states |
| pair-EIM-EIF--EDMrand-APDF | DirectPathUp ×8160 | none | ok | complete, 40225 states |
| pair-EIM-ADF--EIM-EIF | DirectPathUp ×28 | none | ok | complete, 160 states |
| pair-EIM-ADF--EIM-ADF | DirectPathUp ×24 | none | ok | complete, 150 states |
| pair-EIM-ADF--EIM-APDF | DirectPathUp ×24 | none | ok | complete, 150 states |
| pair-EIM-ADF--EDMseq-EIF | DirectPathUp ×46 | none | ok | complete, 282 states |
| pair-EIM-ADF--EDMseq-ADF | DirectPathUp ×36 | none | ok | complete, 232 states |
| pair-EIM-ADF--EDMseq-APDF | DirectPathUp ×36 | none | ok | complete, 232 states |
| pair-EIM-ADF--EDMrand-EIF | DirectPathUp ×5280 | none | ok | complete, 26969 states |
| pair-EIM-ADF--EDMrand-ADF | DirectPathUp ×5088 | none | ok | complete, 25921 states |
| pair-EIM-ADF--EDMrand-APDF | DirectPathUp ×5088 | none | ok | complete, 25921 states |
| pair-EIM-APDF--EIM-EIF | DirectPathUp ×28 | none | ok | complete, 160 states |
| pair-EIM-APDF--EIM-ADF | DirectPathUp ×24 | none | ok | complete, 150 states |
| pair-EIM-APDF--EIM-APDF | DirectPathUp ×24 | none | ok | complete, 150 states |
| pair-EIM-APDF--EDMseq-EIF | DirectPathUp ×56 | none | ok | complete, 357 states |
| pair-EIM-APDF--EDMseq-ADF | DirectPathUp ×36 | none | ok | complete, 243 states |
| pair-EIM-APDF--EDMseq-APDF | DirectPathUp ×36 | none | ok | complete, 243 states |
| pair-EIM-APDF--EDMrand-EIF | DirectPathUp ×8304, RelayedOnly(A:PunchFailed,B:PunchFailed) ×1152, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×1200, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×960 | none | ok | complete, 53909 states |
| pair-EIM-APDF--EDMrand-ADF | DirectPathUp ×6864, RelayedOnly(A:PunchFailed,B:PunchFailed) ×1152, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×1008, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×960 | none | ok | complete, 44689 states |
| pair-EIM-APDF--EDMrand-APDF | DirectPathUp ×6864, RelayedOnly(A:PunchFailed,B:PunchFailed) ×1152, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×1008, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×960 | none | ok | complete, 44689 states |
| pair-EDMseq-APDF--EIM-EIF | DirectPathUp ×56 | none | ok | complete, 297 states |
| pair-EDMseq-APDF--EIM-ADF | DirectPathUp ×68 | none | ok | complete, 368 states |
| pair-EDMseq-APDF--EIM-APDF | DirectPathUp ×68 | none | ok | complete, 379 states |
| pair-EDMseq-APDF--EDMseq-EIF | DirectPathUp ×56 | none | ok | complete, 300 states |
| pair-EDMseq-APDF--EDMseq-ADF | RelayedOnly(A:PunchFailed,B:PunchFailed) ×16, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×15, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×4 | none | ok | complete, 152 states |
| pair-EDMseq-APDF--EDMseq-APDF | RelayedOnly(A:PunchFailed,B:PunchFailed) ×16, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×15, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×4 | none | ok | complete, 152 states |
| pair-EDMseq-APDF--EDMrand-EIF | DirectPathUp ×2688 | none | ok | complete, 13613 states |
| pair-EDMseq-APDF--EDMrand-ADF | RelayedOnly(A:PunchFailed,B:PunchFailed) ×4608, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×2736, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×1152 | none | ok | complete, 23905 states |
| pair-EDMseq-APDF--EDMrand-APDF | RelayedOnly(A:PunchFailed,B:PunchFailed) ×4608, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×2736, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×1152 | none | ok | complete, 23905 states |
| pair-EDMrand-APDF--EIM-EIF | DirectPathUp ×4272 | none | ok | complete, 22658 states |
| pair-EDMrand-APDF--EIM-ADF | DirectPathUp ×6000 | none | ok | complete, 32114 states |
| pair-EDMrand-APDF--EIM-APDF | DirectPathUp ×7968, RelayedOnly(A:PunchFailed,B:PunchFailed) ×1920, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×1128, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×960 | none | ok | complete, 51410 states |
| pair-EDMrand-APDF--EDMseq-EIF | DirectPathUp ×5040 | none | ok | complete, 26786 states |
| pair-EDMrand-APDF--EDMseq-ADF | RelayedOnly(A:PunchFailed,B:PunchFailed) ×1152, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×2736, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×288 | none | ok | complete, 17690 states |
| pair-EDMrand-APDF--EDMseq-APDF | RelayedOnly(A:PunchFailed,B:PunchFailed) ×1152, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×2736, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×288 | none | ok | complete, 17690 states |
| pair-EDMrand-APDF--EDMrand-EIF | DirectPathUp ×84842 | none | ok | PARTIAL (capped), 562799 states |
| pair-EDMrand-APDF--EDMrand-ADF | RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×168497 | none | ok | PARTIAL (capped), 1312168 states |
| pair-EDMrand-APDF--EDMrand-APDF | RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×213135 | none | ok | PARTIAL (capped), 1390073 states |
| observers-1-hard | DirectPathUp ×6864, RelayedOnly(A:PunchFailed,B:PunchFailed) ×1152, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×1008, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×960 | none | ok | complete, 44365 states |
| observers-1-seq | RelayedOnly(A:PunchFailed,B:PunchFailed) ×8, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×7, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×2 | none | ok | complete, 82 states |
| observers-2-hard | DirectPathUp ×6864, RelayedOnly(A:PunchFailed,B:PunchFailed) ×1152, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×1008, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×960 | none | ok | complete, 44689 states |
| observers-2-seq | DirectPathUp ×36 | none | ok | complete, 243 states |
| observers-3-hard | DirectPathUp ×6864, RelayedOnly(A:PunchFailed,B:PunchFailed) ×1152, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×1008, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×960 | none | ok | complete, 44689 states |
| observers-3-seq | DirectPathUp ×36 | none | ok | complete, 243 states |
| observers-4-hard | DirectPathUp ×6864, RelayedOnly(A:PunchFailed,B:PunchFailed) ×1152, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×1008, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×960 | none | ok | complete, 44689 states |
| observers-4-seq | DirectPathUp ×36 | none | ok | complete, 243 states |
| hard-spray-hit | DirectPathUp ×3984 | none | ok | complete, 22057 states |
| hard-spray-miss | DirectPathUp ×2160, RelayedOnly(A:PunchFailed,B:PunchFailed) ×576, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×552, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×480 | none | ok | complete, 16609 states |
| hard-budget-refused | RelayedOnly(A:PunchFailed,B:PunchFailed) ×96, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×84, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×24 | none | ok | complete, 889 states |
| hard-stranger | DirectPathUp ×3516, RelayedOnly(A:PunchFailed,B:PunchFailed) ×864, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×756, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×720 | none | ok | complete, 24266 states |
| hard-latency | DirectPathUp ×41072, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×1172 | none | ok | PARTIAL (capped), 509167 states |
| hard-pooling-arbitrary | RelayedOnly(A:PunchFailed,B:PunchFailed) ×1152, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×792, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×288 | none | ok | complete, 8137 states |
| seq-pooling-arbitrary | RelayedOnly(A:PunchFailed,B:PunchFailed) ×8, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×11, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×2 | none | ok | complete, 110 states |
| hard-accept-all | DirectPathUp ×1080, RelayedOnly(A:PunchFailed,B:PunchFailed) ×480, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×540, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×288 | none | ok | complete, 9589 states |
| gate-accept-all-host-allow | DirectPathUp ×6864, RelayedOnly(A:PunchFailed,B:PunchFailed) ×1152, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×1008, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×960 | none | ok | complete, 44689 states |
| gate-host-deny-b | DirectPathUp ×1080, RelayedOnly(A:PunchFailed,B:PunchFailed) ×480, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×540, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×288 | none | ok | complete, 9589 states |
| gate-host-deny-a | RelayedOnly(A:PunchFailed,B:PunchFailed) ×384, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×264, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×96 | none | ok | complete, 2953 states |
| gate-host-deny-both | RelayedOnly(A:PunchFailed,B:PunchFailed) ×96, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×84, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×24 | none | ok | complete, 889 states |
| gate-identified-only | DirectPathUp ×1080, RelayedOnly(A:PunchFailed,B:PunchFailed) ×480, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×540, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×288 | none | ok | complete, 9589 states |
| gate-identified-only-anonymous | DirectPathUp ×1080, RelayedOnly(A:PunchFailed,B:PunchFailed) ×480, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×540, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×288 | none | ok | complete, 9589 states |
| gate-b-initiated-link | RelayedOnly(A:PunchFailed,B:PunchFailed) ×384, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×264, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×96 | none | ok | complete, 2953 states |
| gate-b-initiated-link-a-allowed | DirectPathUp ×6864, RelayedOnly(A:PunchFailed,B:PunchFailed) ×1152, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×1008, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×960 | none | ok | complete, 44689 states |
| gate-random-proposer-b-initiated | DirectPathUp ×2064, RelayedOnly(A:PunchFailed,B:PunchFailed) ×672, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×348, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×504 | none | ok | complete, 14102 states |
| sockets-ramp-two-observer-K2 | DirectPathUp ×13920 | none | ok | complete, 75913 states |
| sockets-ramp-fallback-K2 | DirectPathUp ×13920 | none | ok | complete, 75397 states |
| forged-probe | RelayedOnly(A:PunchFailed,B:PunchFailed) ×8, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×7, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×2 | none | ok | complete, 82 states |
| forged-probe-hard | RelayedOnly(A:PunchFailed,B:PunchFailed) ×1152, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×792, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×288 | none | ok | complete, 8137 states |
| easy-glare-a-wins | DirectPathUp ×72, RelayedOnly(A:Timeout,B:Glare) ×1, RelayedOnly(A:Timeout,B:PunchFailed) ×8 | none | ok | complete, 626 states |
| easy-glare-b-wins | DirectPathUp ×72, RelayedOnly(A:Glare,B:Timeout) ×1, RelayedOnly(A:PunchFailed,B:Timeout) ×8 | none | ok | complete, 626 states |
| easy-policy-reject | RelayedOnly(A:Rejected(POLICY),B:-) ×1 | none | ok | complete, 5 states |
| easy-no-facilitator-answer | RelayedOnly(A:ProbeFailed,B:-) ×1 | none | ok | complete, 3 states |
| easy-relay-lossy | DirectPathUp ×24, RelayedOnly(A:Rejected(PUNCH_FAILED),B:Timeout) ×84, RelayedOnly(A:Timeout,B:-) ×2, RelayedOnly(A:Timeout,B:PunchFailed) ×8, RelayedOnly(A:Timeout,B:Rejected(PUNCH_FAILED)) ×12, RelayedOnly(A:Timeout,B:Timeout) ×72 | NoLoneMove | ok | complete, 1082 states |
| easy-relay-dup | DirectPathUp ×24 | none | ok | complete, 150 states |
| easy-relay-slow | DirectPathUp ×24, RelayedOnly(A:Rejected(PUNCH_FAILED),B:Timeout) ×24, RelayedOnly(A:Timeout,B:Timeout) ×108 | NoLoneMove | ok | complete, 606 states |
| samenat-hairpin-relay-slow | DirectPathUp ×264, RelayedOnly(A:Rejected(PUNCH_FAILED),B:Timeout) ×230, RelayedOnly(A:Timeout,B:Timeout) ×1188 | NoLoneMove | ok | complete, 6069 states |
| easy-driver-slow | DirectPathUp ×24, RelayedOnly(A:PunchFailed,B:PunchFailed) ×1292, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×235, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×186 | none | ok | complete, 2266 states |
| hard-relay-slow | DirectPathUp ×340, RelayedOnly(A:Rejected(PUNCH_FAILED),B:Timeout) ×284, RelayedOnly(A:Timeout,B:Timeout) ×581 | NoLoneMove | ok | complete, 13784 states |
| easy-net-lossy | DirectPathUp ×298, RelayedOnly(A:PunchFailed,B:PunchFailed) ×136, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×387, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×58 | none | ok | complete, 3568 states |
| easy-teardown | DirectPathUp ×24, LinkClosed ×146 | none | ok | complete, 297 states |
| easy-keepalive-lost | DirectPathUp ×96 | none | ok | complete, 258 states |
| easy-own-facilitators-differ | DirectPathUp ×24 | none | ok | complete, 150 states |
| samenat-hairpin | DirectPathUp ×264 | none | ok | complete, 1155 states |
| samenat-hairpin-before-nomination | DirectPathUp ×95 | SamePath | ok | complete, 580 states |
| samenat-hairpin-net-lossy | DirectPathUp ×11570, RelayedOnly(A:PunchFailed,B:PunchFailed) ×3184, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×9091, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×2064 | none | ok | complete, 115150 states |
| hard-net-lossy | DirectPathUp ×48432, RelayedOnly(A:PunchFailed,B:PunchFailed) ×35232, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×47304, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×22872 | none | ok | complete, 547657 states |
| samenat-no-hairpin | DirectPathUp ×58 | none | ok | complete, 330 states |
| samenat-hard-no-hairpin | DirectPathUp ×2736 | none | ok | complete, 14585 states |
| skew-1 | DirectPathUp ×17424, RelayedOnly(A:PunchFailed,B:PunchFailed) ×6624, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×6480, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×5112 | none | ok | complete, 129681 states |
| skew-1-seq | DirectPathUp ×130, RelayedOnly(A:PunchFailed,B:PunchFailed) ×36, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×35, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×29 | none | ok | complete, 999 states |
| mac-EDMseq-APDF--EDMseq-ADF | DirectPathUp ×60 | none | ok | complete, 335 states |
| mac-EDMseq-APDF--EDMseq-APDF | RelayedOnly(A:PunchFailed,B:PunchFailed) ×16, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×15, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×4 | none | ok | complete, 152 states |
| mac-EDMseq-APDF--EDMrand-ADF | DirectPathUp ×28032 | none | ok | complete, 128161 states |
| mac-EDMseq-APDF--EDMrand-APDF | RelayedOnly(A:PunchFailed,B:PunchFailed) ×4608, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×2736, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×1152 | none | ok | complete, 23905 states |
| mac-EDMrand-APDF--EDMseq-ADF | DirectPathUp ×1344 | none | ok | complete, 7754 states |
| mac-EDMrand-APDF--EDMseq-APDF | RelayedOnly(A:PunchFailed,B:PunchFailed) ×1152, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×2736, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×288 | none | ok | complete, 17690 states |
| mac-EDMrand-APDF--EDMrand-ADF | DirectPathUp ×59824 | none | ok | PARTIAL (capped), 951487 states |
| mac-EDMrand-APDF--EDMrand-APDF | DirectPathUp ×70423, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×13532 | none | ok | PARTIAL (capped), 1220682 states |
| mac-hard-stranger | DirectPathUp ×2736 | none | ok | complete, 14468 states |
| residual5-edm-eif | DirectPathUp ×8304, RelayedOnly(A:PunchFailed,B:PunchFailed) ×1152, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×1200, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×960 | none | ok | complete, 53909 states |
| residual5-edm-eif-fallback | DirectPathUp ×8064, RelayedOnly(A:PunchFailed,B:PunchFailed) ×1152, RelayedOnly(A:PunchFailed,B:Rejected(PUNCH_FAILED)) ×1200, RelayedOnly(A:Rejected(PUNCH_FAILED),B:PunchFailed) ×960 | none | ok | complete, 52441 states |
| plant-hijack | - | StrangerKnowledge | VIOLATED | complete, 1351 states |
| plant-named-facilitator | - | FacilitatorKnowledge | ok | complete, 150 states |
