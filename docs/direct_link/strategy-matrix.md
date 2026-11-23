| scenario | outcomes (final states) | properties violated | states |
|---|---|---|---|
| strategy--ambiguous-trusted | direct:bdaySym x16, relayed x32 | none | 864 |
| strategy--ambiguous-untrusted | relayed x64 | none | 576 |
| strategy--anon-initiator | relayed x32 | none | 144 |
| strategy--anon-responder | relayed x32 | none | 144 |
| strategy--bothrandom-mac-reachable | relayed x64 | none | 576 |
| strategy--bothrandom-nothing | relayed x64 | none | 576 |
| strategy--budget-one-hard | direct:bdayB x16, relayed x32 | none | 864 |
| strategy--budget-zero-hard | relayed x64 | none | 576 |
| strategy--fac-silent-a-upnp | direct:mapB x16, relayed x32 | none | 576 |
| strategy--fac-silent-a-v6 | direct:v6 x32 | none | 576 |
| strategy--fac-silent-b | relayed x32 | none | 192 |
| strategy--fault-net-lossy | direct:plain x1152, relayed x3200 | none | 24800 |
| strategy--fault-net-lossy-hard | direct:bdayB x576, relayed x1632 | none | 12976 |
| strategy--fault-relay-lossy | direct:plain x1536, relayed x1952 | none | 34432 |
| strategy--fault-relay-lossy-hard | direct:bdayB x1024, relayed x2528 | none | 38224 |
| strategy--gate-deny-a-seq | relayed x64 | none | 576 |
| strategy--gate-deny-b-hard | relayed x64 | none | 576 |
| strategy--hard-filtknown | direct:bdayB x16, relayed x32 | none | 576 |
| strategy--hard-unclassified-eif | direct:bdayB x16, direct:plain x32, relayed x32 | none | 1472 |
| strategy--hard-unclassified-eimrandalloc | direct:bdaySym x16, relayed x32 | none | 864 |
| strategy--hello-one-side | direct:bdayB x16, relayed x32 | none | 864 |
| strategy--lan-leak-cgn-mac | direct:hair x32 | none | 576 |
| strategy--lan-leak-cgn-raw | direct:hair x32 | none | 576 |
| strategy--learn-pathloss | direct:plain x64, relayed x32 | none | 1408 |
| strategy--learn-pathloss-hard | direct:bdayB x48, relayed x64 | none | 1632 |
| strategy--learn-three-attempts-hard | direct:bdayB x16, relayed x32 | none | 864 |
| strategy--pair-EDMrand-ADF--EDMrand-APDF-clsF | relayed x64 | none | 864 |
| strategy--pair-EDMrand-ADF--EDMrand-APDF-clsT | relayed x64 | none | 576 |
| strategy--pair-EDMrand-ADF--EDMseq-APDF-clsF | relayed x64 | none | 864 |
| strategy--pair-EDMrand-ADF--EDMseq-APDF-clsT | relayed x64 | none | 576 |
| strategy--pair-EDMrand-ADF--EIM-APDF-clsF | direct:bdayA x16, relayed x32 | none | 864 |
| strategy--pair-EDMrand-ADF--EIM-APDF-clsT | direct:bdayA x16, relayed x32 | none | 864 |
| strategy--pair-EDMrand-APDF--EDMrand-APDF-clsF | relayed x64 | none | 864 |
| strategy--pair-EDMrand-APDF--EDMrand-APDF-clsT | relayed x64 | none | 576 |
| strategy--pair-EDMrand-APDF--EDMrand-EIF-clsF | direct:plain x32 | none | 752 |
| strategy--pair-EDMrand-APDF--EDMrand-EIF-clsT | direct:plain x32 | none | 752 |
| strategy--pair-EDMrand-APDF--EDMseq-APDF-clsF | relayed x64 | none | 864 |
| strategy--pair-EDMrand-APDF--EDMseq-APDF-clsT | relayed x64 | none | 576 |
| strategy--pair-EDMrand-APDF--EDMseq-EIF-clsF | direct:plain x32 | none | 752 |
| strategy--pair-EDMrand-APDF--EDMseq-EIF-clsT | direct:plain x32 | none | 752 |
| strategy--pair-EDMrand-APDF--EIM-APDF-clsF | direct:bdayA x16, relayed x32 | none | 864 |
| strategy--pair-EDMrand-APDF--EIM-APDF-clsT | direct:bdayA x16, relayed x32 | none | 864 |
| strategy--pair-EDMrand-APDF--EIM-EIF-clsF | direct:plain x32 | none | 752 |
| strategy--pair-EDMrand-APDF--EIM-EIF-clsT | direct:plain x32 | none | 752 |
| strategy--pair-EDMseq-ADF--EDMrand-APDF-clsF | relayed x64 | none | 864 |
| strategy--pair-EDMseq-ADF--EDMrand-APDF-clsT | relayed x64 | none | 576 |
| strategy--pair-EDMseq-ADF--EDMseq-APDF-clsF | relayed x64 | none | 576 |
| strategy--pair-EDMseq-ADF--EDMseq-APDF-clsT | relayed x64 | none | 576 |
| strategy--pair-EDMseq-ADF--EIM-APDF-clsF | relayed x64 | DirectWhenPossible | 576 |
| strategy--pair-EDMseq-ADF--EIM-APDF-clsT | direct:predA x16, relayed x32 | none | 944 |
| strategy--pair-EDMseq-APDF--EDMrand-APDF-clsF | relayed x64 | none | 864 |
| strategy--pair-EDMseq-APDF--EDMrand-APDF-clsT | relayed x64 | none | 576 |
| strategy--pair-EDMseq-APDF--EDMrand-EIF-clsF | direct:plain x32 | none | 752 |
| strategy--pair-EDMseq-APDF--EDMrand-EIF-clsT | direct:plain x32 | none | 752 |
| strategy--pair-EDMseq-APDF--EDMseq-APDF-clsF | relayed x64 | none | 576 |
| strategy--pair-EDMseq-APDF--EDMseq-APDF-clsT | relayed x64 | none | 576 |
| strategy--pair-EDMseq-APDF--EDMseq-EIF-clsF | direct:plain x32 | none | 752 |
| strategy--pair-EDMseq-APDF--EDMseq-EIF-clsT | direct:plain x32 | none | 752 |
| strategy--pair-EDMseq-APDF--EIM-APDF-clsF | relayed x64 | DirectWhenPossible | 576 |
| strategy--pair-EDMseq-APDF--EIM-APDF-clsT | direct:predA x16, relayed x32 | none | 944 |
| strategy--pair-EDMseq-APDF--EIM-EIF-clsF | direct:plain x32 | none | 752 |
| strategy--pair-EDMseq-APDF--EIM-EIF-clsT | direct:plain x32 | none | 752 |
| strategy--pair-EIM-ADF--EDMrand-APDF-clsF | direct:plain x32 | none | 752 |
| strategy--pair-EIM-ADF--EDMrand-APDF-clsT | direct:plain x32 | none | 752 |
| strategy--pair-EIM-ADF--EDMseq-APDF-clsF | direct:plain x32 | none | 752 |
| strategy--pair-EIM-ADF--EDMseq-APDF-clsT | direct:plain x32 | none | 752 |
| strategy--pair-EIM-ADF--EIM-APDF-clsF | direct:plain x32 | none | 576 |
| strategy--pair-EIM-ADF--EIM-APDF-clsT | direct:plain x32 | none | 576 |
| strategy--pair-EIM-APDF--EDMrand-APDF-clsF | direct:bdayB x16, relayed x32 | none | 864 |
| strategy--pair-EIM-APDF--EDMrand-APDF-clsT | direct:bdayB x16, relayed x32 | none | 864 |
| strategy--pair-EIM-APDF--EDMrand-EIF-clsF | direct:bdayB x16, direct:plain x32, relayed x32 | none | 1472 |
| strategy--pair-EIM-APDF--EDMrand-EIF-clsT | direct:bdayB x16, direct:plain x32, relayed x32 | none | 1472 |
| strategy--pair-EIM-APDF--EDMseq-APDF-clsF | relayed x64 | DirectWhenPossible | 576 |
| strategy--pair-EIM-APDF--EDMseq-APDF-clsT | direct:predB x16, relayed x32 | none | 944 |
| strategy--pair-EIM-APDF--EDMseq-EIF-clsF | direct:plain x32, direct:predB x16, relayed x32 | none | 1552 |
| strategy--pair-EIM-APDF--EDMseq-EIF-clsT | direct:plain x32, direct:predB x16, relayed x32 | none | 1552 |
| strategy--pair-EIM-APDF--EIM-APDF-clsF | direct:plain x32 | none | 576 |
| strategy--pair-EIM-APDF--EIM-APDF-clsT | direct:plain x32 | none | 576 |
| strategy--pair-EIM-APDF--EIM-EIF-clsF | direct:plain x32 | none | 576 |
| strategy--pair-EIM-APDF--EIM-EIF-clsT | direct:plain x32 | none | 576 |
| strategy--plant-downgrade | direct:plain x32, relayed x96 | NoRawToCapablePeer, NoReplay | 896 |
| strategy--plant-hijack | relayed x17 | NoTokenToStranger, StrangerKnowledge | 161 |
| strategy--plant-named-facilitator | direct:plain x32 | FacilitatorKnowledge | 576 |
| strategy--plant-untagged | direct:bdayB x26624, relayed x53888 | NoRace | 847232 |
| strategy--portblock-hard | direct:bdayB x16, relayed x32 | none | 864 |
| strategy--replay-onpath-mac | direct:plain x32, relayed x32 | none | 704 |
| strategy--replay-onpath-raw | direct:plain x32, relayed x96 | NoReplay | 896 |
| strategy--reversal-eim-eif-known | direct:rev x32 | none | 576 |
| strategy--samecgn-cand | direct:lan x32 | none | 576 |
| strategy--samecgn-nocand-hairpin | direct:hair x32 | none | 576 |
| strategy--samecgn-nocand-nohairpin | relayed x64 | none | 576 |
| strategy--samelan-hairpin | direct:lan x32 | none | 576 |
| strategy--samelan-nohairpin | direct:lan x32 | none | 576 |
| strategy--seq-unclassified-eif | direct:plain x32, direct:predB x16, relayed x32 | none | 1552 |
| strategy--seq-vs-random | relayed x64 | none | 576 |
| strategy--seqseq-mac | direct:predAB x16, relayed x32 | none | 864 |
| strategy--seqseq-mac-bothstrict | relayed x64 | none | 864 |
| strategy--seqseq-raw | relayed x64 | none | 576 |
| strategy--shaped-hard | direct:bdayB x16, relayed x32 | none | 864 |
| strategy--stranger-hard | direct:bdayB x32, relayed x64 | none | 1328 |
| strategy--tranches-hard | direct:bdayB x112, relayed x128 | none | 5760 |
| strategy--tranches-hard-budget2 | direct:bdayB x48, relayed x128 | none | 3456 |
| strategy--upnp-bothrandom | direct:mapA x16, relayed x32 | none | 864 |
| strategy--upnp-easy-side | direct:bdayB x8, direct:mapA x16, relayed x16 | none | 1008 |
| strategy--upnp-hard-side | direct:bdayB x8, direct:mapB x16, relayed x16 | none | 1008 |
| strategy--v6-both | direct:v6 x32 | none | 576 |
| strategy--v6-both-bothrandom | direct:v6 x32 | none | 576 |
| strategy--v6-both-closed-hard | direct:bdayB x16, relayed x32 | none | 1152 |
| strategy--v6-both-hard | direct:v6 x32 | none | 576 |
| strategy--v6-one-side | direct:plain x32 | none | 576 |
| today--ambiguous-trusted | direct:bdayB x48, relayed x288 | EscPerAttempt, NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 2368 |
| today--ambiguous-untrusted | relayed x320 | EscPerAttempt, NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 2496 |
| today--anon-initiator | direct:bdayB x48, relayed x288 | NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction | 2368 |
| today--anon-responder | direct:plain x1248, direct:v6 x1248 | Agreement, OneTechnique | 13984 |
| today--bothrandom-mac-reachable | direct:bdayA x624, direct:bdayB x624, relayed x288 | Agreement, EscPerAttempt, NoNegligibleRow, NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 8896 |
| today--bothrandom-nothing | relayed x576 | EscPerAttempt, NoNegligibleRow, NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 3712 |
| today--budget-one-hard | relayed x640 | DirectWhenPossible, NoFalseAbort, NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 4768 |
| today--budget-zero-hard | relayed x320 | NoSpam, NotRetriedSameWay, RetryIsEvidenceFunction | 2496 |
| today--fac-silent-a-upnp | relayed x48 | DirectWhenPossible | 288 |
| today--fac-silent-a-v6 | relayed x48 | DirectWhenPossible | 288 |
| today--fac-silent-b | relayed x48 | none | 288 |
| today--fault-net-lossy | direct:plain x288, relayed x1216 | none | 9984 |
| today--fault-net-lossy-hard | direct:bdayB x144, relayed x672 | OneTechnique, UsesEvidence | 4704 |
| today--fault-relay-lossy | direct:plain x768, relayed x2240 | none | 23904 |
| today--fault-relay-lossy-hard | direct:bdayB x384, relayed x1328 | OneTechnique, UsesEvidence | 14048 |
| today--gate-deny-a-seq | relayed x320 | NoSpam, NotRetriedSameWay, RetryIsEvidenceFunction | 2496 |
| today--gate-deny-b-hard | relayed x320 | NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 2496 |
| today--hard-filtknown | direct:bdayB x48, relayed x288 | NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction | 2368 |
| today--hard-unclassified-eif | direct:plain x96, relayed x320 | DirectWhenPossible, NoFalseAbort, NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 3520 |
| today--hard-unclassified-eimrandalloc | direct:bdayB x48, relayed x288 | EscPerAttempt, NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 2368 |
| today--hello-one-side | direct:bdayB x48, relayed x288 | NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 2368 |
| today--lan-leak-cgn-mac | direct:hair x192 | NoSpam, OneTechnique | 1760 |
| today--lan-leak-cgn-raw | direct:hair x192 | NoSpam, NoTokenToStranger, OneTechnique, StrangerKnowledge | 1760 |
| today--learn-pathloss | direct:plain x384, relayed x288 | none | 5248 |
| today--learn-pathloss-hard | direct:bdayB x48, relayed x672 | DirectWhenPossible, NoFalseAbort, NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 4720 |
| today--learn-three-attempts-hard | direct:bdayB x48, relayed x672 | NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 5696 |
| today--pair-EDMrand-ADF--EDMrand-APDF-clsF | relayed x576 | EscPerAttempt, NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 3712 |
| today--pair-EDMrand-ADF--EDMrand-APDF-clsT | relayed x576 | EscPerAttempt, NoNegligibleRow, NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 3712 |
| today--pair-EDMrand-ADF--EDMseq-APDF-clsF | relayed x576 | NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 3712 |
| today--pair-EDMrand-ADF--EDMseq-APDF-clsT | relayed x576 | EscPerAttempt, NoNegligibleRow, NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 3712 |
| today--pair-EDMrand-ADF--EIM-APDF-clsF | direct:bdayA x48, relayed x288 | NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 2368 |
| today--pair-EDMrand-ADF--EIM-APDF-clsT | direct:bdayA x48, relayed x288 | NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 2368 |
| today--pair-EDMrand-APDF--EDMrand-APDF-clsF | relayed x576 | EscPerAttempt, NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 3712 |
| today--pair-EDMrand-APDF--EDMrand-APDF-clsT | relayed x576 | EscPerAttempt, NoNegligibleRow, NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 3712 |
| today--pair-EDMrand-APDF--EDMrand-EIF-clsF | direct:plain x96 | none | 928 |
| today--pair-EDMrand-APDF--EDMrand-EIF-clsT | direct:plain x96 | none | 928 |
| today--pair-EDMrand-APDF--EDMseq-APDF-clsF | relayed x576 | NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 3712 |
| today--pair-EDMrand-APDF--EDMseq-APDF-clsT | relayed x576 | EscPerAttempt, NoNegligibleRow, NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 3712 |
| today--pair-EDMrand-APDF--EDMseq-EIF-clsF | direct:plain x96 | none | 928 |
| today--pair-EDMrand-APDF--EDMseq-EIF-clsT | direct:plain x96 | none | 928 |
| today--pair-EDMrand-APDF--EIM-APDF-clsF | direct:bdayA x48, relayed x288 | NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 2368 |
| today--pair-EDMrand-APDF--EIM-APDF-clsT | direct:bdayA x48, relayed x288 | NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 2368 |
| today--pair-EDMrand-APDF--EIM-EIF-clsF | direct:plain x96 | none | 928 |
| today--pair-EDMrand-APDF--EIM-EIF-clsT | direct:plain x96 | none | 928 |
| today--pair-EDMseq-ADF--EDMrand-APDF-clsF | relayed x576 | NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 3712 |
| today--pair-EDMseq-ADF--EDMrand-APDF-clsT | relayed x576 | EscPerAttempt, NoNegligibleRow, NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 3712 |
| today--pair-EDMseq-ADF--EDMseq-APDF-clsF | relayed x320 | NoSpam, NotRetriedSameWay, RetryIsEvidenceFunction | 2496 |
| today--pair-EDMseq-ADF--EDMseq-APDF-clsT | relayed x576 | EscPerAttempt, NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 3712 |
| today--pair-EDMseq-ADF--EIM-APDF-clsF | relayed x320 | DirectWhenPossible, NoSpam, NotRetriedSameWay, RetryIsEvidenceFunction | 2496 |
| today--pair-EDMseq-ADF--EIM-APDF-clsT | direct:predA x48, relayed x160 | NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 1952 |
| today--pair-EDMseq-APDF--EDMrand-APDF-clsF | relayed x576 | NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 3712 |
| today--pair-EDMseq-APDF--EDMrand-APDF-clsT | relayed x576 | EscPerAttempt, NoNegligibleRow, NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 3712 |
| today--pair-EDMseq-APDF--EDMrand-EIF-clsF | direct:plain x96 | none | 928 |
| today--pair-EDMseq-APDF--EDMrand-EIF-clsT | direct:plain x96 | none | 928 |
| today--pair-EDMseq-APDF--EDMseq-APDF-clsF | relayed x320 | NoSpam, NotRetriedSameWay, RetryIsEvidenceFunction | 2496 |
| today--pair-EDMseq-APDF--EDMseq-APDF-clsT | relayed x576 | EscPerAttempt, NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 3712 |
| today--pair-EDMseq-APDF--EDMseq-EIF-clsF | direct:plain x96 | none | 928 |
| today--pair-EDMseq-APDF--EDMseq-EIF-clsT | direct:plain x96 | none | 928 |
| today--pair-EDMseq-APDF--EIM-APDF-clsF | relayed x320 | DirectWhenPossible, NoSpam, NotRetriedSameWay, RetryIsEvidenceFunction | 2496 |
| today--pair-EDMseq-APDF--EIM-APDF-clsT | direct:predA x48, relayed x160 | NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 1952 |
| today--pair-EDMseq-APDF--EIM-EIF-clsF | direct:plain x96 | none | 928 |
| today--pair-EDMseq-APDF--EIM-EIF-clsT | direct:plain x96 | none | 928 |
| today--pair-EIM-ADF--EDMrand-APDF-clsF | direct:plain x96 | none | 928 |
| today--pair-EIM-ADF--EDMrand-APDF-clsT | direct:plain x96 | none | 928 |
| today--pair-EIM-ADF--EDMseq-APDF-clsF | direct:plain x96 | none | 928 |
| today--pair-EIM-ADF--EDMseq-APDF-clsT | direct:plain x96 | none | 928 |
| today--pair-EIM-ADF--EIM-APDF-clsF | direct:plain x96 | none | 928 |
| today--pair-EIM-ADF--EIM-APDF-clsT | direct:plain x96 | none | 928 |
| today--pair-EIM-APDF--EDMrand-APDF-clsF | direct:bdayB x48, relayed x288 | NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 2368 |
| today--pair-EIM-APDF--EDMrand-APDF-clsT | direct:bdayB x48, relayed x288 | NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 2368 |
| today--pair-EIM-APDF--EDMrand-EIF-clsF | direct:plain x96, relayed x320 | DirectWhenPossible, NoFalseAbort, NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 3520 |
| today--pair-EIM-APDF--EDMrand-EIF-clsT | direct:plain x96, relayed x320 | DirectWhenPossible, NoFalseAbort, NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 3520 |
| today--pair-EIM-APDF--EDMseq-APDF-clsF | relayed x320 | DirectWhenPossible, NoSpam, NotRetriedSameWay, RetryIsEvidenceFunction | 2496 |
| today--pair-EIM-APDF--EDMseq-APDF-clsT | direct:predB x48, relayed x160 | NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 1952 |
| today--pair-EIM-APDF--EDMseq-EIF-clsF | direct:plain x96, relayed x320 | DirectWhenPossible, NoSpam, NotRetriedSameWay, RetryIsEvidenceFunction | 3520 |
| today--pair-EIM-APDF--EDMseq-EIF-clsT | direct:plain x96, direct:predB x48, relayed x160 | NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 2944 |
| today--pair-EIM-APDF--EIM-APDF-clsF | direct:plain x96 | none | 928 |
| today--pair-EIM-APDF--EIM-APDF-clsT | direct:plain x96 | none | 928 |
| today--pair-EIM-APDF--EIM-EIF-clsF | direct:plain x96 | none | 928 |
| today--pair-EIM-APDF--EIM-EIF-clsT | direct:plain x96 | none | 928 |
| today--plant-hijack | relayed x355 | NoPathToStranger, NoSpam, NoTokenToStranger, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, StrangerKnowledge, UsesEvidence | 2320 |
| today--plant-named-facilitator | direct:plain x96 | FacilitatorKnowledge | 928 |
| today--portblock-hard | direct:bdayB x48, relayed x288 | NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 2368 |
| today--replay-onpath-mac | direct:plain x96, relayed x96 | none | 1312 |
| today--replay-onpath-raw | direct:plain x96, relayed x288 | NoReplay | 1888 |
| today--reversal-eim-eif-known | direct:plain x96 | UsesEvidence | 928 |
| today--samecgn-cand | direct:hair x1248, direct:lan x1248 | Agreement, OneTechnique, UsesEvidence | 13984 |
| today--samecgn-nocand-hairpin | direct:hair x96 | NoSpam, OneTechnique | 928 |
| today--samecgn-nocand-nohairpin | relayed x320 | NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction | 2496 |
| today--samelan-hairpin | direct:hair x1248, direct:lan x1248 | Agreement, OneTechnique, UsesEvidence | 13984 |
| today--samelan-nohairpin | direct:lan x96 | OneTechnique, UsesEvidence | 928 |
| today--seq-unclassified-eif | direct:plain x96, relayed x320 | DirectWhenPossible, NoSpam, NotRetriedSameWay, RetryIsEvidenceFunction | 3520 |
| today--seq-vs-random | relayed x576 | EscPerAttempt, NoNegligibleRow, NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 3712 |
| today--seqseq-mac | relayed x576 | DirectWhenPossible, EscPerAttempt, NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 3712 |
| today--seqseq-mac-bothstrict | relayed x576 | EscPerAttempt, NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 3712 |
| today--seqseq-raw | relayed x576 | EscPerAttempt, NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 3712 |
| today--shaped-hard | direct:bdayB x48, relayed x288 | NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 2368 |
| today--stranger-hard | direct:bdayB x96, relayed x576 | NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 4544 |
| today--tranches-hard | direct:bdayB x48, relayed x2016 | DirectWhenPossible, NoFalseAbort, NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 16064 |
| today--tranches-hard-budget2 | direct:bdayB x48, relayed x1056 | NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 9248 |
| today--upnp-bothrandom | relayed x576 | DirectWhenPossible, EscPerAttempt, NoNegligibleRow, NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 3712 |
| today--upnp-easy-side | direct:bdayB x48, relayed x288 | DirectWhenPossible, NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 2368 |
| today--upnp-hard-side | direct:bdayB x48, relayed x288 | DirectWhenPossible, NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 2368 |
| today--v6-both | direct:plain x1248, direct:v6 x1248 | Agreement, OneTechnique, UsesEvidence | 13984 |
| today--v6-both-bothrandom | direct:v6 x96 | OneTechnique, UsesEvidence | 928 |
| today--v6-both-closed-hard | direct:bdayB x48, relayed x288 | NoSpam, NotRetriedSameWay, OneTechnique, RetryIsEvidenceFunction, UsesEvidence | 2368 |
| today--v6-both-hard | direct:v6 x96 | OneTechnique, UsesEvidence | 928 |
| today--v6-one-side | direct:plain x96 | NoSpam, OneTechnique | 928 |
