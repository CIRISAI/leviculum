| pair (A--B) | DirectLinkPunch terminal states (outcome, A seen, B seen) | oracle | agree |
|---|---|---|---|
| EIM-EIF--EIM-EIF | [('direct', (True, True, True, False), (True, True, True, False))] | [('direct', (True, True, True, False), (True, True, True, False))] | yes |
| EIM-EIF--EIM-ADF | [('direct', (True, True, True, False), (True, True, True, False))] | [('direct', (True, True, True, False), (True, True, True, False))] | yes |
| EIM-EIF--EIM-APDF | [('direct', (True, True, True, False), (True, True, True, False))] | [('direct', (True, True, True, False), (True, True, True, False))] | yes |
| EIM-EIF--EDMseq-EIF | [('direct', (True, True, True, False), (True, True, True, False)), ('direct', (True, True, True, True), (True, True, True, False))] | [('direct', (True, True, True, False), (True, True, True, False)), ('direct', (True, True, True, True), (True, True, True, False))] | yes |
| EIM-EIF--EDMseq-ADF | [('direct', (True, True, True, True), (True, True, True, False))] | [('direct', (True, True, True, True), (True, True, True, False))] | yes |
| EIM-EIF--EDMseq-APDF | [('direct', (True, True, True, True), (True, True, True, False))] | [('direct', (True, True, True, True), (True, True, True, False))] | yes |
| EIM-EIF--EDMrand-EIF | [('direct', (True, True, True, False), (True, True, True, False)), ('direct', (True, True, True, True), (True, True, True, False))] | [('direct', (True, True, True, False), (True, True, True, False)), ('direct', (True, True, True, True), (True, True, True, False))] | yes |
| EIM-EIF--EDMrand-ADF | [('direct', (True, True, True, True), (True, True, True, False))] | [('direct', (True, True, True, True), (True, True, True, False))] | yes |
| EIM-EIF--EDMrand-APDF | [('direct', (True, True, True, True), (True, True, True, False))] | [('direct', (True, True, True, True), (True, True, True, False))] | yes |
| EIM-ADF--EIM-EIF | [('direct', (True, True, True, False), (True, True, True, False))] | [('direct', (True, True, True, False), (True, True, True, False))] | yes |
| EIM-ADF--EIM-ADF | [('direct', (True, True, True, False), (True, True, True, False))] | [('direct', (True, True, True, False), (True, True, True, False))] | yes |
| EIM-ADF--EIM-APDF | [('direct', (True, True, True, False), (True, True, True, False))] | [('direct', (True, True, True, False), (True, True, True, False))] | yes |
| EIM-ADF--EDMseq-EIF | [('direct', (True, True, True, False), (True, True, True, False)), ('direct', (True, True, True, True), (True, True, True, False))] | [('direct', (True, True, True, False), (True, True, True, False)), ('direct', (True, True, True, True), (True, True, True, False))] | yes |
| EIM-ADF--EDMseq-ADF | [('direct', (True, True, True, True), (True, True, True, False))] | [('direct', (True, True, True, True), (True, True, True, False))] | yes |
| EIM-ADF--EDMseq-APDF | [('direct', (True, True, True, True), (True, True, True, False))] | [('direct', (True, True, True, True), (True, True, True, False))] | yes |
| EIM-ADF--EDMrand-EIF | [('direct', (True, True, True, False), (True, True, True, False)), ('direct', (True, True, True, True), (True, True, True, False))] | [('direct', (True, True, True, False), (True, True, True, False)), ('direct', (True, True, True, True), (True, True, True, False))] | yes |
| EIM-ADF--EDMrand-ADF | [('direct', (True, True, True, True), (True, True, True, False))] | [('direct', (True, True, True, True), (True, True, True, False))] | yes |
| EIM-ADF--EDMrand-APDF | [('direct', (True, True, True, True), (True, True, True, False))] | [('direct', (True, True, True, True), (True, True, True, False))] | yes |
| EIM-APDF--EIM-EIF | [('direct', (True, True, True, False), (True, True, True, False))] | [('direct', (True, True, True, False), (True, True, True, False))] | yes |
| EIM-APDF--EIM-ADF | [('direct', (True, True, True, False), (True, True, True, False))] | [('direct', (True, True, True, False), (True, True, True, False))] | yes |
| EIM-APDF--EIM-APDF | [('direct', (True, True, True, False), (True, True, True, False))] | [('direct', (True, True, True, False), (True, True, True, False))] | yes |
| EIM-APDF--EDMseq-EIF | [('direct', (True, True, True, False), (True, True, True, False)), ('relayed', (False, False, True, False), (True, False, False, False))] | [('direct', (True, True, True, False), (True, True, True, False)), ('relayed', (False, False, False, False), (True, False, False, False))] | yes, after the ack artifact |
| EIM-APDF--EDMseq-ADF | [('relayed', (False, False, False, False), (False, False, False, False))] | [('relayed', (False, False, False, False), (False, False, False, False))] | yes |
| EIM-APDF--EDMseq-APDF | [('relayed', (False, False, False, False), (False, False, False, False))] | [('relayed', (False, False, False, False), (False, False, False, False))] | yes |
| EIM-APDF--EDMrand-EIF | [('direct', (True, True, True, False), (True, True, True, False)), ('relayed', (False, False, True, False), (True, False, False, False))] | [('direct', (True, True, True, False), (True, True, True, False)), ('relayed', (False, False, False, False), (True, False, False, False))] | yes, after the ack artifact |
| EIM-APDF--EDMrand-ADF | [('relayed', (False, False, False, False), (False, False, False, False))] | [('relayed', (False, False, False, False), (False, False, False, False))] | yes |
| EIM-APDF--EDMrand-APDF | [('relayed', (False, False, False, False), (False, False, False, False))] | [('relayed', (False, False, False, False), (False, False, False, False))] | yes |
| EDMseq-EIF--EIM-EIF | [('direct', (True, True, True, False), (True, True, True, False)), ('direct', (True, True, True, False), (True, True, True, True))] | [('direct', (True, True, True, False), (True, True, True, False)), ('direct', (True, True, True, False), (True, True, True, True))] | yes |
| EDMseq-EIF--EIM-ADF | [('direct', (True, True, True, False), (True, True, True, False)), ('direct', (True, True, True, False), (True, True, True, True))] | [('direct', (True, True, True, False), (True, True, True, False)), ('direct', (True, True, True, False), (True, True, True, True))] | yes |
| EDMseq-EIF--EIM-APDF | [('direct', (True, True, True, False), (True, True, True, False)), ('relayed', (True, False, False, False), (False, False, True, False))] | [('direct', (True, True, True, False), (True, True, True, False)), ('relayed', (True, False, False, False), (False, False, False, False))] | yes, after the ack artifact |
| EDMseq-EIF--EDMseq-EIF | [('direct', (True, True, True, False), (True, True, True, True)), ('direct', (True, True, True, True), (True, True, True, False)), ('direct', (True, True, True, True), (True, True, True, True))] | [('direct', (True, True, True, False), (True, True, True, False))] | outcome yes; other-port seen is a race |
| EDMseq-EIF--EDMseq-ADF | [('direct', (True, True, True, True), (True, True, True, False))] | [('direct', (True, True, True, True), (True, True, True, False))] | yes |
| EDMseq-EIF--EDMseq-APDF | [('direct', (True, True, True, True), (True, True, True, False))] | [('direct', (True, True, True, True), (True, True, True, False))] | yes |
| EDMseq-EIF--EDMrand-EIF | [('direct', (True, True, True, False), (True, True, True, True)), ('direct', (True, True, True, True), (True, True, True, False)), ('direct', (True, True, True, True), (True, True, True, True))] | [('direct', (True, True, True, False), (True, True, True, False))] | outcome yes; other-port seen is a race |
| EDMseq-EIF--EDMrand-ADF | [('direct', (True, True, True, True), (True, True, True, False))] | [('direct', (True, True, True, True), (True, True, True, False))] | yes |
| EDMseq-EIF--EDMrand-APDF | [('direct', (True, True, True, True), (True, True, True, False))] | [('direct', (True, True, True, True), (True, True, True, False))] | yes |
| EDMseq-ADF--EIM-EIF | [('direct', (True, True, True, False), (True, True, True, True))] | [('direct', (True, True, True, False), (True, True, True, True))] | yes |
| EDMseq-ADF--EIM-ADF | [('direct', (True, True, True, False), (True, True, True, True))] | [('direct', (True, True, True, False), (True, True, True, True))] | yes |
| EDMseq-ADF--EIM-APDF | [('relayed', (False, False, False, False), (False, False, False, False))] | [('relayed', (False, False, False, False), (False, False, False, False))] | yes |
| EDMseq-ADF--EDMseq-EIF | [('direct', (True, True, True, False), (True, True, True, True))] | [('direct', (True, True, True, False), (True, True, True, True))] | yes |
| EDMseq-ADF--EDMseq-ADF | [('relayed', (False, False, False, False), (False, False, False, False))] | [('relayed', (False, False, False, False), (False, False, False, False))] | yes |
| EDMseq-ADF--EDMseq-APDF | [('relayed', (False, False, False, False), (False, False, False, False))] | [('relayed', (False, False, False, False), (False, False, False, False))] | yes |
| EDMseq-ADF--EDMrand-EIF | [('direct', (True, True, True, False), (True, True, True, True))] | [('direct', (True, True, True, False), (True, True, True, True))] | yes |
| EDMseq-ADF--EDMrand-ADF | [('relayed', (False, False, False, False), (False, False, False, False))] | [('relayed', (False, False, False, False), (False, False, False, False))] | yes |
| EDMseq-ADF--EDMrand-APDF | [('relayed', (False, False, False, False), (False, False, False, False))] | [('relayed', (False, False, False, False), (False, False, False, False))] | yes |
| EDMseq-APDF--EIM-EIF | [('direct', (True, True, True, False), (True, True, True, True))] | [('direct', (True, True, True, False), (True, True, True, True))] | yes |
| EDMseq-APDF--EIM-ADF | [('direct', (True, True, True, False), (True, True, True, True))] | [('direct', (True, True, True, False), (True, True, True, True))] | yes |
| EDMseq-APDF--EIM-APDF | [('relayed', (False, False, False, False), (False, False, False, False))] | [('relayed', (False, False, False, False), (False, False, False, False))] | yes |
| EDMseq-APDF--EDMseq-EIF | [('direct', (True, True, True, False), (True, True, True, True))] | [('direct', (True, True, True, False), (True, True, True, True))] | yes |
| EDMseq-APDF--EDMseq-ADF | [('relayed', (False, False, False, False), (False, False, False, False))] | [('relayed', (False, False, False, False), (False, False, False, False))] | yes |
| EDMseq-APDF--EDMseq-APDF | [('relayed', (False, False, False, False), (False, False, False, False))] | [('relayed', (False, False, False, False), (False, False, False, False))] | yes |
| EDMseq-APDF--EDMrand-EIF | [('direct', (True, True, True, False), (True, True, True, True))] | [('direct', (True, True, True, False), (True, True, True, True))] | yes |
| EDMseq-APDF--EDMrand-ADF | [('relayed', (False, False, False, False), (False, False, False, False))] | [('relayed', (False, False, False, False), (False, False, False, False))] | yes |
| EDMseq-APDF--EDMrand-APDF | [('relayed', (False, False, False, False), (False, False, False, False))] | [('relayed', (False, False, False, False), (False, False, False, False))] | yes |
| EDMrand-EIF--EIM-EIF | [('direct', (True, True, True, False), (True, True, True, False)), ('direct', (True, True, True, False), (True, True, True, True))] | [('direct', (True, True, True, False), (True, True, True, False)), ('direct', (True, True, True, False), (True, True, True, True))] | yes |
| EDMrand-EIF--EIM-ADF | [('direct', (True, True, True, False), (True, True, True, False)), ('direct', (True, True, True, False), (True, True, True, True))] | [('direct', (True, True, True, False), (True, True, True, False)), ('direct', (True, True, True, False), (True, True, True, True))] | yes |
| EDMrand-EIF--EIM-APDF | [('direct', (True, True, True, False), (True, True, True, False)), ('relayed', (True, False, False, False), (False, False, True, False))] | [('direct', (True, True, True, False), (True, True, True, False)), ('relayed', (True, False, False, False), (False, False, False, False))] | yes, after the ack artifact |
| EDMrand-EIF--EDMseq-EIF | [('direct', (True, True, True, False), (True, True, True, True)), ('direct', (True, True, True, True), (True, True, True, False)), ('direct', (True, True, True, True), (True, True, True, True))] | [('direct', (True, True, True, False), (True, True, True, False))] | outcome yes; other-port seen is a race |
| EDMrand-EIF--EDMseq-ADF | [('direct', (True, True, True, True), (True, True, True, False))] | [('direct', (True, True, True, True), (True, True, True, False))] | yes |
| EDMrand-EIF--EDMseq-APDF | [('direct', (True, True, True, True), (True, True, True, False))] | [('direct', (True, True, True, True), (True, True, True, False))] | yes |
| EDMrand-EIF--EDMrand-EIF | [('direct', (True, True, True, False), (True, True, True, True)), ('direct', (True, True, True, True), (True, True, True, False)), ('direct', (True, True, True, True), (True, True, True, True))] | [('direct', (True, True, True, False), (True, True, True, False))] | outcome yes; other-port seen is a race |
| EDMrand-EIF--EDMrand-ADF | [('direct', (True, True, True, True), (True, True, True, False))] | [('direct', (True, True, True, True), (True, True, True, False))] | yes |
| EDMrand-EIF--EDMrand-APDF | [('direct', (True, True, True, True), (True, True, True, False))] | [('direct', (True, True, True, True), (True, True, True, False))] | yes |
| EDMrand-ADF--EIM-EIF | [('direct', (True, True, True, False), (True, True, True, True))] | [('direct', (True, True, True, False), (True, True, True, True))] | yes |
| EDMrand-ADF--EIM-ADF | [('direct', (True, True, True, False), (True, True, True, True))] | [('direct', (True, True, True, False), (True, True, True, True))] | yes |
| EDMrand-ADF--EIM-APDF | [('relayed', (False, False, False, False), (False, False, False, False))] | [('relayed', (False, False, False, False), (False, False, False, False))] | yes |
| EDMrand-ADF--EDMseq-EIF | [('direct', (True, True, True, False), (True, True, True, True))] | [('direct', (True, True, True, False), (True, True, True, True))] | yes |
| EDMrand-ADF--EDMseq-ADF | [('relayed', (False, False, False, False), (False, False, False, False))] | [('relayed', (False, False, False, False), (False, False, False, False))] | yes |
| EDMrand-ADF--EDMseq-APDF | [('relayed', (False, False, False, False), (False, False, False, False))] | [('relayed', (False, False, False, False), (False, False, False, False))] | yes |
| EDMrand-ADF--EDMrand-EIF | [('direct', (True, True, True, False), (True, True, True, True))] | [('direct', (True, True, True, False), (True, True, True, True))] | yes |
| EDMrand-ADF--EDMrand-ADF | [('relayed', (False, False, False, False), (False, False, False, False))] | [('relayed', (False, False, False, False), (False, False, False, False))] | yes |
| EDMrand-ADF--EDMrand-APDF | [('relayed', (False, False, False, False), (False, False, False, False))] | [('relayed', (False, False, False, False), (False, False, False, False))] | yes |
| EDMrand-APDF--EIM-EIF | [('direct', (True, True, True, False), (True, True, True, True))] | [('direct', (True, True, True, False), (True, True, True, True))] | yes |
| EDMrand-APDF--EIM-ADF | [('direct', (True, True, True, False), (True, True, True, True))] | [('direct', (True, True, True, False), (True, True, True, True))] | yes |
| EDMrand-APDF--EIM-APDF | [('relayed', (False, False, False, False), (False, False, False, False))] | [('relayed', (False, False, False, False), (False, False, False, False))] | yes |
| EDMrand-APDF--EDMseq-EIF | [('direct', (True, True, True, False), (True, True, True, True))] | [('direct', (True, True, True, False), (True, True, True, True))] | yes |
| EDMrand-APDF--EDMseq-ADF | [('relayed', (False, False, False, False), (False, False, False, False))] | [('relayed', (False, False, False, False), (False, False, False, False))] | yes |
| EDMrand-APDF--EDMseq-APDF | [('relayed', (False, False, False, False), (False, False, False, False))] | [('relayed', (False, False, False, False), (False, False, False, False))] | yes |
| EDMrand-APDF--EDMrand-EIF | [('direct', (True, True, True, False), (True, True, True, True))] | [('direct', (True, True, True, False), (True, True, True, True))] | yes |
| EDMrand-APDF--EDMrand-ADF | [('relayed', (False, False, False, False), (False, False, False, False))] | [('relayed', (False, False, False, False), (False, False, False, False))] | yes |
| EDMrand-APDF--EDMrand-APDF | [('relayed', (False, False, False, False), (False, False, False, False))] | [('relayed', (False, False, False, False), (False, False, False, False))] | yes |
