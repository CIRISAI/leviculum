//! The #375 simulation as a host test: N boards with random addresses,
//! random arrival orders, the REAL initiation rule
//! ([`leviculum_ble_tx::should_initiate`]) and the real slot limits —
//! three incoming links, one outgoing, a full board stops advertising
//! (#372), a board whose outgoing link is up stops scanning, waiting
//! boards rescan every round, and the fallback switches a board's scan
//! mode after a bounded number of empty rounds, exactly as the
//! firmware's clock does after `SCAN_FALLBACK_AFTER_MS`.
//!
//! The harness is calibrated against the issue's own Monte Carlo: under
//! the strict rule it reproduces the published disconnection rates
//! exactly (21 % of orders at 10 boards, 40 % at 20 — see the control
//! below). Part 2 of the batch turned it into a 2×2 instrument over the
//! two open policy questions:
//!
//! - **When may the fallback fire?** [`FallbackSpec::Eager`]: the clock
//!   runs whenever the outgoing slot is free — the shipped spec since
//!   part 3. [`FallbackSpec::Quiet`]: only while the board has no live
//!   link in either role — shipped briefly in part 2, kept here as the
//!   record.
//! - **Which eligible advertiser is dialled?**
//!   [`TargetChoice::FirstSeen`]: whichever eligible PDU the radio
//!   heard first (as first built; seeded random here).
//!   [`TargetChoice::LowestEligible`]: collect one scan window and dial
//!   the lowest-addressed candidate, strict verdicts before fallback
//!   verdicts, via the same [`CandidateTable`] the firmware and lnsd
//!   use — the policy is shared by construction.
//!
//! Item 3 added a third question — **which of several free peers?** —
//! and with it [`TargetChoice::MostFreeSlots`]: every board advertises
//! how many incoming slots it still has, and the window prefers the
//! emptiest one, equal counts falling back to the address as before.
//!
//! #412 added a fourth — **what breaks a tie the address should not
//! decide?** — and with it [`TargetChoice::FallbackFirstHeard`] and
//! [`TargetChoice::RotatingLast`]; see the churn section below.
//!
//! Measured on this seed stream (disconnected/linkless orders per 1000,
//! and saturated boards — boards that ended with all three incoming
//! slots spent — summed over the same 1000 orders):
//!
//! | spec  | choice   | n=10           | n=20            |
//! |-------|----------|----------------|-----------------|
//! | eager | first    | 2 / 0 / 1514   | 48 / 0 / 3632   |
//! | eager | lowest   | 0 / 0 / 976    | 0 / 0 / 2538    |
//! | eager | mostfree | 0 / 0 / 498    | 0 / 0 / 914     |
//! | quiet | first    | 126 / 0 / 1462 | 328 / 0 / 3496  |
//! | quiet | lowest   | 28 / 0 / 976   | 78 / 0 / 2488   |
//! | strict (control) | 210 / 84 / 1450 | 400 / 80 / 3486 |
//!
//! What the assertions below hold on to:
//!
//! - **The lowest-eligible window closes the saturated-cycle lock**:
//!   eager/lowest is 0/1000 at both sizes, and under quiet it cuts the
//!   splits by ~4× against first-seen.
//! - **The slot preference does not regress convergence and halves
//!   saturation**: eager/mostfree stays at 0/1000 disconnected and
//!   0/1000 linkless at both sizes, and the boards that end with every
//!   incoming slot spent fall from 976 to 498 at n=10 and from 2538 to
//!   914 at n=20. Saturation is the quantity item 3 is about: the sim
//!   reads a peer's capacity directly instead of dialling and being
//!   refused, so the refusals themselves are invisible here, but every
//!   one of them happens at a board the searchers piled onto — and the
//!   pile-ups are what halved.
//! - **The quiet spec has a real, bounded cost in this harness**:
//!   28/1000 and 78/1000 disconnected orders against eager/lowest's
//!   0/0 — well beyond noise. The mechanism: quiet suppresses exactly
//!   the cross-component merge dial. A component whose boards all hold
//!   SOME link (so their clocks are suspended) but lose the sort
//!   against the other component's advertisers can never initiate the
//!   merge, and when no strict edge exists in either direction the two
//!   components are stable disjoint. Every residual split is of this
//!   all-linked kind — the linkless column stays 0, so #375's headline
//!   failure (a board with no BLE link at all, scanning forever) never
//!   returns.
//!
//! Part 2 shipped quiet anyway, because the rig had shown an eager
//! fallback dialling a peer it was already linked to every ~20 s,
//! forever (§0 of the 2026-09-09 batch): a dial the Core Spec dooms
//! (Vol 6 Part B §4.5, one connection per address pair), spent on air
//! every cycle. Part 3 shipped eager after all — that doomed-dial
//! cycle is now closed at its root, not by the clock: a live
//! connection's address is excluded before it can leave the scanner
//! (the registry's `addr_linked`, the §4.5 exclusion) and a fallback
//! dial that cannot even connect goes into the dead-end table for two
//! minutes. With the cycle gone, quiet bought nothing this table does
//! not take away, and the 28/78 all-linked splits it cost are exactly
//! the merges eager performs.
//!
//! # A churning peer in the room (Codeberg #412)
//!
//! Everything above is an empty room of boards. #412 is what happens
//! when an Android Columba is in it: on the corpus night of
//! 2026-09-14/15 `feld-t114` made 7 outgoing links in two days and all
//! 7 went to the phone, none to a board; `t114-boot` 12 of 22. The
//! harness gained a [`Churn`] parameter for it — one identity, a new
//! address every 48 s (the capture's five addresses in four minutes),
//! always advertising, central-capable, links ending at the next
//! rotation and expiring `LINK_TIMEOUT_MS` later, which is the
//! capture's `reason="timeout"`. Rounds became worth 5 s
//! ([`ROUND_MS`], the firmware's own search-connect-backoff cycle) so
//! that those periods mean something, and the run goes to a fixed
//! ten-minute horizon instead of to quiescence, because a room with a
//! churning peer in it is never quiescent.
//!
//! Zero churning peers reproduces every pre-#412 number exactly — that
//! is asserted, not hoped: the churn state draws from a seed stream of
//! its own, so no draw of the original moved.
//!
//! Measured (per 1000 orders; `disc` = split board graphs, `bb` =
//! board-to-board links formed, `%churn` = share of all dials aimed at
//! a churning peer, `d/bb` = dials per board-to-board link):
//!
//! | churn | policy         | n=10 disc / bb / %churn / d-bb | n=20 disc / bb / %churn / d-bb |
//! |------:|----------------|--------------------------------|--------------------------------|
//! |     0 | strict         |    210 /  8790 /  0 % / 1.00   |    400 / 18582 /  0 % / 1.00   |
//! |     0 | eager/first    |      2 / 10000 /  0 % / 1.00   |     48 / 20000 /  0 % / 1.00   |
//! |     0 | eager/lowest   |      0 / 10000 /  0 % / 1.00   |      0 / 20000 /  0 % / 1.00   |
//! |     0 | quiet/lowest   |     28 /  8972 /  0 % / 1.00   |     78 / 18922 /  0 % / 1.00   |
//! |     0 | eager/mostfree |      0 / 10000 /  0 % / 1.00   |      0 / 20000 /  0 % / 1.00   |
//! |     1 | strict         |     88 /  8912 /  0 % / 1.00   |    336 / 18660 /  0 % / 1.00   |
//! |     1 | eager/first    |     12 / 10000 /  2 % / 1.02   |     64 / 20000 /  1 % / 1.01   |
//! |     1 | eager/lowest   |     32 /  8996 / 41 % / 1.71   |    140 / 18894 / 27 % / 1.37   |
//! |     1 | quiet/lowest   |     32 /  8968 /  2 % / 1.02   |    114 / 18886 /  2 % / 1.02   |
//! |     1 | eager/mostfree |     44 /  8984 / 42 % / 1.72   |    180 / 18854 / 28 % / 1.38   |
//! |     2 | strict         |    110 /  8890 /  0 % / 1.00   |    314 / 18674 /  0 % / 1.00   |
//! |     2 | eager/first    |     42 / 10000 /  5 % / 1.05   |     48 / 20000 /  2 % / 1.02   |
//! |     2 | eager/lowest   |     80 /  8920 / 43 % / 1.75   |    166 / 18840 / 27 % / 1.38   |
//! |     2 | quiet/lowest   |     80 /  8920 /  3 % / 1.03   |    170 / 18828 /  3 % / 1.03   |
//! |     2 | eager/mostfree |     86 /  8914 / 43 % / 1.76   |    194 / 18808 / 28 % / 1.39   |
//!
//! (`quiet/first` is in the test's printed table; it behaves like
//! `quiet/lowest` on every column that matters here.)
//!
//! ## The shipped policy degrades, and the reason is the sort
//!
//! `eager/mostfree` goes from 0 split graphs per 1000 orders to 44 at
//! ten boards and 180 at twenty — at twenty boards that is 45 % of the
//! entire connectivity win #375 bought. Board-to-board links fall by
//! 10 % and 5.7 %, and boards that end with no link to any board at all
//! go from none to 28 and 48 per 1000 orders.
//!
//! The mechanism is not congestion and not chance. A resolvable
//! private address has `01` in its top two bits and a SoftDevice static
//! random address has `11` (`peer.rs`:
//! `any_rpa_sorts_below_any_static_random_address`), so a phone is
//! ALWAYS below every board numerically. It is therefore never a strict
//! candidate — the `strict` rows spend 0 % of their dials on it, which
//! is the model's own positive control — and it is ALWAYS the first
//! candidate of the fallback class, which [`CandidateTable`] orders by
//! address. Every fallback dial of every board in the room goes to the
//! phone, and 48 s later its address is new and it wins again. 42 % of
//! the room's dials end there at ten boards, 28 % at twenty, against
//! the rig's 20 of 37.
//!
//! ## The address-ordered window IS the mechanism
//!
//! `eager/first` — what the firmware did before #375 item 2 — spends
//! 2 % of its dials on the churning peer and loses not one
//! board-to-board link. It dials an arbitrary eligible advertiser, so
//! its fallback dials spread across the room; the window concentrates
//! them all on the single lowest address, and with a phone in the room
//! that address is the phone, every window, every rotation. The
//! preference added by item 3 cannot help: free slots order candidates
//! INSIDE a class, and a phone that advertises no capability record at
//! all is ranked "all slots free" by construction (`window.rs` says why,
//! and the reason is still right — ranking silence as zero would demote
//! every phone below every board).
//!
//! This is not an argument for going back to first-seen: first-seen is
//! what the saturated-cycle lock was made of (2 and 48 splits in an
//! empty room, and #375 §0's doomed re-dial cycle). It is an argument
//! that the fallback class must be ordered by something other than the
//! raw address.
//!
//! ## What the fallback class is ordered by instead
//!
//! Two candidate orders, both in [`leviculum_ble_tx::FallbackOrder`],
//! both measured on this instrument with everything else held equal
//! (same seeds, same phones, same spec: only the tie-break moves):
//!
//! | churn | policy             | n=10 disc / boardless / bb / %churn | n=20 disc / boardless / bb / %churn |
//! |------:|--------------------|-------------------------------------|-------------------------------------|
//! |     0 | eager/mostfree     |   0 /  0 / 10000 /  0 %             |   0 /  0 / 20000 /  0 %             |
//! |     0 | eager/firstheard   |   2 /  0 / 10000 /  0 %             |  74 /  0 / 20000 /  0 %             |
//! |     0 | eager/rotatinglast |   0 /  0 / 10000 /  0 %             |   0 /  0 / 20000 /  0 %             |
//! |     1 | eager/mostfree     |  44 / 28 /  8984 / 42 %             | 180 / 48 / 18854 / 28 %             |
//! |     1 | eager/firstheard   |   6 /  0 /  9866 / 13 %             |  58 /  2 / 19998 /  3 %             |
//! |     1 | eager/rotatinglast |  22 /  2 /  9912 /  6 %             |  78 /  0 / 20000 /  0 %             |
//! |     2 | eager/mostfree     |  86 / 26 /  8914 / 43 %             | 194 / 46 / 18808 / 28 %             |
//! |     2 | eager/firstheard   |  36 /  8 /  9664 / 24 %             |  42 /  0 / 19996 /  5 %             |
//! |     2 | eager/rotatinglast |  24 / 16 /  9796 / 12 %             |  94 /  0 / 19996 /  0 %             |
//!
//! **First-heard** drops the address term for every candidate and
//! breaks the tie by which advertiser the window heard first. It works
//! against the phone, and it hands back most of the saturated-cycle
//! lock in the empty room — 74 split graphs per 1000 at twenty boards
//! against zero, which is the #375 guarantee itself. That is not a
//! surprise in hindsight: the agreement between searchers that the
//! address order produced is what closed the lock, and an arbitrary
//! tie-break has no agreement in it. The row stays as the record of
//! the trade.
//!
//! **Rotating-last**, what ships, keeps the address term where the
//! address is a key and drops it where it is not: a candidate whose
//! address class says its owner redraws it (`01` or `00` on top, Core
//! Spec Vol 6 Part B §1.3.2.2) ranks behind every candidate whose does
//! not, first-heard among themselves. Every board in a room of boards
//! has a static random address, so the empty room is not merely as
//! good but BIT-IDENTICAL, which the table above asserts as an
//! equality rather than a bound.
//!
//! With one phone in the room it takes the mechanism out: the share of
//! the room's dials aimed at the phone falls from 42 % to 6 % at ten
//! boards and from 28 % to 0 % at twenty, the board-to-board links
//! come back to within 1 % of the empty room (and exactly to it at
//! twenty), the boards left with no board link at all fall from 28 and
//! 48 to 2 and 0, and the split graphs at least halve. It is a
//! preference and not an exclusion, which the two-phone row shows by
//! still spending 12 % of its dials there — a board with nobody else
//! to dial dials the phone, as it must.
//!
//! What it does NOT do, and first-heard partly does: spread the
//! fallback dials among the BOARDS. That is why first-heard still wins
//! some churned cells (6 against 22 at ten boards with one phone)
//! while paying for it in the empty room. Whether the fallback class
//! should be spread among boards as well is #375's question, not this
//! one.
//!
//! The residual is the address class itself: a PUBLIC address carries
//! no constraint on its top bits, so a BlueZ host whose OUI begins
//! below `0x80` reads as rotating and is ordered behind the boards
//! inside the fallback class. It costs such a peer position, never a
//! dial. The address TYPE arrives with every advertising report and
//! would settle it exactly; plumbing it through both stacks is the
//! honest fix and it is not this one.
//!
//! ## The quiet spec, which part 3 dropped, also resists
//!
//! `quiet/lowest` suspends the fallback clock while the board holds any
//! link, and a board holding a phone's incoming link therefore never
//! reaches fallback: 2 % of dials spent on churn instead of 41 %, and at
//! twenty boards FEWER split graphs than eager (114 against 140). The
//! ranking of part 3's decision inverts with a phone in the room. The
//! quiet spec's known cost — 28 and 78 all-linked splits in an empty
//! room — is the price of that, and it is smaller than what churn costs
//! eager.
//!
//! It is not what ships, because it pays that cost in every room and
//! the fallback order above pays none: a board holding a phone's
//! incoming link never reaching fallback at all is a blunt version of
//! not electing the phone.
//!
//! ## One row improves, and it is a confound, not good news
//!
//! The `strict` row gets BETTER with a churning peer: 210 split graphs
//! per 1000 down to 88. The strict rule can never dial the peer, so the
//! only thing left is its incoming links — every slot it holds makes
//! that board go dark one board-link earlier, which spreads the
//! dialling load, which is exactly the quantity #375 item 3 optimises
//! (at n=10, boards ending with every incoming slot spent: 1450 in the
//! empty room, 1006 with a dialling churn peer). A churning peer is an accidental load-spreader while it robs
//! the fallback. Both directions are pinned in
//! `control_the_churn_model_is_a_parameter_and_every_mechanism_fires`
//! so that no future reader takes an improving churn row for a defence.
//!
//! ## Everything above is the formation phase
//!
//! In every table so far a board-to-board link never ends: boards do not
//! rotate, sessions do not drop, nothing reboots. So a board that won a
//! board link never dials again, and those numbers are the
//! FORMATION-phase cost of a churning peer. The rig measured the
//! steady-state one — `feld-t114` had its outgoing slot free seven times
//! over two days and the phone won all seven — and 100 % is worse than
//! this harness's 42 % for exactly that reason. [`Mortality`] is that
//! missing half, and the next section is what it measures.
//!
//! # Steady state, link mortality
//!
//! [`Mortality`] gives a board-to-board link a drawn lifetime; when it
//! elapses the link ends at both ends in the same round, the central's
//! one outgoing slot and the peripheral's incoming slot free together,
//! and both boards re-enter the scan under today's rules. The parameter's
//! zero is the formation phase: every table above is measured at
//! `Mortality::IMMORTAL` and every number in them is pinned as an
//! equality, which is what makes "zero mortality changes nothing" a
//! measurement rather than a hope. Where the distribution comes from,
//! what one lifetime stands for on a board, and the two limits of the
//! capture it is read off are in [`Mortality`]'s own docs; the two rows
//! are its short mode ([`Mortality::CAPTURE_SHORT_MODE`], mean 45 s,
//! measured median 37.2 s) and its tail ([`Mortality::CAPTURE_TAIL`],
//! mean 600 s, measured p90 188.8 s).
//!
//! `freed%` is the share of the dials that spent a FREED outgoing slot
//! which went to a churning peer — the steady-state quantity the
//! formation tables have no equivalent of — and `top%` is that same
//! share for the HIGHEST-ADDRESSED board alone. The distinction is the
//! whole reading, and the captures state it in the boards' own verdicts
//! rather than by inference. The three static addresses are
//! `feld-pocket` `dcb18ad99cdb` < `t114-boot` `e1ac9eb3f05c` <
//! `feld-t114` `e81d77f09cdc`, and each board's `BLE_SCAN_DECISION`
//! lines follow: `feld-t114` logged `rule=wait_peer_lower_address` for
//! both other boards 2301 times and `rule=initiate_lower_address` for
//! neither of them ONCE, so it has no strict board candidate in that
//! room at all and every dial it makes is a fallback dial;
//! `feld-pocket`, the lowest, logged `initiate_lower_address` for both.
//! `feld-t114`'s 7 of 7 is that board's number, not a room average, and
//! the room's own average that night was 20 of 37.
//! `solo%` is how many of the top board's churn dials had no board on
//! offer at all. `bb` counts links FORMED (in a mortal room that is far
//! more than the links standing), `d/use` is dials per link that lasted,
//! `disc` is split board graphs as a snapshot at the horizon.
//!
//! The room the rig had — three boards, one dialling phone (per 1000
//! orders):
//!
//! | policy             | life     | freed% | top%  | solo% | bb    | d/use | disc |
//! |--------------------|----------|-------:|------:|------:|------:|------:|-----:|
//! | eager/mostfree     | immortal | 100 %  | 100 % |  57 % |  2040 | 2.04  |    0 |
//! | eager/mostfree     | 45 s     |   6 %  | 100 % |   3 % | 26790 | 1.40  |    0 |
//! | eager/mostfree     | 600 s    |  57 %  | 100 % |  38 % |  3904 | 1.84  |    0 |
//! | eager/rotatinglast | immortal | 100 %  | 100 % |  57 % |  2040 | 2.04  |    0 |
//! | eager/rotatinglast | 45 s     |   6 %  | 100 % |   3 % | 26790 | 1.40  |    0 |
//! | eager/rotatinglast | 600 s    |  57 %  | 100 % |  38 % |  3904 | 1.84  |    0 |
//!
//! Ten and twenty boards, 0 / 1 / 2 churning peers (`-` where no slot
//! was ever freed, which is what an immortal room with nobody churning
//! is):
//!
//! | n  | churn | policy             | life     | freed% | top%  | bb     | d/use | disc |
//! |---:|------:|--------------------|----------|-------:|------:|-------:|------:|-----:|
//! | 10 |     0 | either             | immortal |    -   |   -   |  10000 | 1.00  |    0 |
//! | 10 |     0 | either             | 45 s     |   0 %  |   0 % | 122812 | 1.27  |    0 |
//! | 10 |     0 | either             | 600 s    |   0 %  |   0 % |  19270 | 1.03  |   12 |
//! | 10 |     1 | eager/mostfree     | immortal |   99 % | 100 % |   8984 | 1.03  |   44 |
//! | 10 |     1 | eager/mostfree     | 45 s     |    3 % | 100 % | 116844 | 1.27  |    0 |
//! | 10 |     1 | eager/mostfree     | 600 s    |   37 % | 100 % |  17514 | 1.05  |    4 |
//! | 10 |     1 | eager/rotatinglast | immortal |   83 % |  86 % |   9912 | 1.00  |   22 |
//! | 10 |     1 | eager/rotatinglast | 45 s     |    3 % |  79 % | 118040 | 1.27  |    0 |
//! | 10 |     1 | eager/rotatinglast | 600 s    |   12 % |  68 % |  18804 | 1.03  |   14 |
//! | 10 |     2 | eager/mostfree     | 45 s     |    3 % | 100 % | 116562 | 1.27  |    0 |
//! | 10 |     2 | eager/rotatinglast | 45 s     |    3 % |  84 % | 117496 | 1.27  |    0 |
//! | 20 |     0 | either             | immortal |    -   |   -   |  20000 | 1.00  |    0 |
//! | 20 |     0 | either             | 45 s     |   0 %  |   0 % | 242126 | 1.27  |    0 |
//! | 20 |     1 | eager/mostfree     | immortal |   97 % | 100 % |  18854 | 1.02  |  180 |
//! | 20 |     1 | eager/mostfree     | 45 s     |    2 % | 100 % | 236242 | 1.27  |    0 |
//! | 20 |     1 | eager/mostfree     | 600 s    |   22 % | 100 % |  36260 | 1.04  |   18 |
//! | 20 |     1 | eager/rotatinglast | immortal |    0 % |   0 % |  20000 | 1.00  |   78 |
//! | 20 |     1 | eager/rotatinglast | 45 s     |    1 % |  51 % | 239166 | 1.27  |    0 |
//! | 20 |     1 | eager/rotatinglast | 600 s    |    0 % |   7 % |  38008 | 1.03  |   42 |
//! | 20 |     2 | eager/mostfree     | 45 s     |    2 % | 100 % | 235948 | 1.27  |    0 |
//! | 20 |     2 | eager/rotatinglast | 45 s     |    1 % |  50 % | 238916 | 1.27  |    0 |
//!
//! (The two policies differ only in the fallback tie-break, so the
//! churn-0 rows are one row: identical in every column, as they are in
//! the empty-room table above. The test prints every cell.)
//!
//! ## Does the harness reproduce the rig's 100 %? For that board, yes
//!
//! **Yes for the board the rig read, and only for that board.** In the
//! rig's room under the rig's policy the highest-addressed board's freed
//! outgoing slot goes to the phone 100.00 % of the time — at every
//! mortality level, immortal included — against `feld-t114`'s 7 of 7.
//! The room AVERAGE in the same cell is 6 % at the short-mode row and
//! 57 % at the tail row, and that spread is not a disagreement with the
//! capture but the same split the capture has: the two lower-addressed
//! boards have a strict board candidate and re-link to a board, exactly
//! as `feld-pocket` did (1 of 8 to the phone) and `t114-boot` half did
//! (12 of 22), while the board with no strict candidate spends every dial
//! on the phone. The room's 20 of 37 that night sits between the
//! harness's room average and its 100 % top board, which is what an
//! average over those two populations looks like — and it is the reason
//! the top-board column exists: a room average cannot be held against a
//! one-board measurement.
//!
//! ## The shipped fallback order does not defend that slot, and why
//!
//! **Superseded in its conclusion by the last section of these docs, and
//! kept because its mechanism is still exactly right**: every row below
//! is a room of boards and phones, and the rig's room had a fourth
//! member. With that member in it the shipped order DOES defend the slot
//! (0.32 % to the phone, `Statics`). What follows is what the key does
//! when a phone is the only thing on offer, which is still the room a
//! mesh of boards in one flat is when no fourth node is up.
//!
//! In the rig's own room `eager/rotatinglast` is 100 % too — identical
//! in every column to the address order it replaced. On the capture's
//! short-mode row it is NOT because there was nothing else to dial:
//! `solo%` is 3 %, so a board was on offer in 97 % of those windows and
//! lost. (The other two rows carry a second reason on top, and it is a
//! reason no candidate order can address either: with links that stand,
//! a three-board room has every board excluded by the §4.5 address rule
//! already, so 57 % of the immortal row's churn dials and 38 % of the
//! 600 s row's had no board on offer at all.) The key is
//! (tier, free-slot deficit, rotating group, address or sighting) and the
//! DEFICIT sits above the group: a phone advertises no capability record
//! at all, which reads as "all slots free" and deficits by zero, while a
//! board carrying even one incoming link deficits by one. In a
//! three-board room every board is carrying one, so the phone wins the
//! second term before the fourth is ever consulted.
//! `a_board_that_has_spent_an_incoming_slot_loses_the_fallback_to_a_silent_phone`
//! is that window on its own, through the real rule and the real table.
//!
//! The order does bite where a board with slots to spare still exists:
//! at twenty boards with one phone it takes the top board's share from
//! 100 % to 51 % at the 45 s row and to 7 % at 600 s. What it cannot do
//! is defend the last board in a small room, and a small room is what a
//! mesh of boards in one flat is.
//!
//! ## The tier from #412 part 1 is worth exactly zero here, measured
//!
//! `DialPreference::CannotDialUs` never receives a candidate: across
//! every cell of the table above, 0 of 21 066 436 window offers reach
//! that tier. The reason is structural and not a seed — no advertisement
//! in the model carries `CAP_PERIPHERAL_ONLY`, because a board's
//! `LOCAL_CAPS` is 0 (`columba.rs:213`, lnsd's `links.rs:47`) and a phone
//! carries no record at all — so a row measured with the tier switched
//! off would be the same row twice. The tier is counted instead of
//! switched, which is the stronger statement: it says the tier had
//! nothing to promote, not merely that the totals matched. Part 1 was
//! never about a phone; it is about a peripheral-only PEER, and until
//! something in the room advertises that bit the tier cannot defend a
//! freed slot in any room this harness can build.
//!
//! ## The direction to quote carefully
//!
//! For the room as a whole, mortality LOWERS the share of dials spent on
//! a churning peer: at n=10 with one phone and the address order, 41.7 %
//! of all dials in the formation phase against 4.0 % in the steady state
//! (at n=20, 27.7 % against 1.9 %), and the `freed%` column falls from
//! 99 % to 3 % with it — while `top%` stays at 100 %. A death hands both boards a strict candidate back
//! and they re-link in the next round, so most freed slots are spent on a
//! board.
//! The rig's steady-state number is worse than the formation table's
//! because of the BOARD it was read on, not because sessions end. And
//! `bb` rising from 10 000 to 122 812 per 1000 orders is a count of
//! formations, not of connectivity; `disc` is the connectivity column
//! and it stays at 0 in a room of boards.
//!
//! **Flipped 2026-09-27 (#412, [`ConnectFailure`]).** That last clause is
//! a statement about a room where every dial connects. At the measured
//! board-to-board failure rate the same room of ten boards at the same
//! 45 s mean splits in 508 orders of 1000 and leaves 280 boards per 1000
//! orders with no board link at all; at twenty boards, 820 and 416. `bb`
//! falls 13 %. The formation phase survives (10 000 links, `disc` 0 at
//! ten boards; 20 000 and 14 at twenty), because a dial that failed is a
//! dial the board makes again and nothing else in the room is moving. It
//! is the steady state that does not: a death frees the slot, the re-dial
//! lands two times in three, and each miss costs the board two to three
//! rounds in which it is neither scanning nor linked. The rest of this
//! section is unaffected — the direction it warns about is the one it
//! still has.
//!
//!
//! ## What this harness still cannot see
//!
//! Four differences from a board remain, each stated with what it is
//! worth rather than ranked, because nothing here measures which of them
//! carries the residual:
//!
//! - ~~**Every dial connects.**~~ **Closed 2026-09-27 by
//!   [`ConnectFailure`]**, and it was the largest of the five: it was
//!   load-bearing for the `disc` column of every table above, not for
//!   the churn columns the section it heads is about. The last section
//!   of these docs is the re-measurement. What the limit said is still
//!   the measurement it was read off: `feld-t114` logged 758
//!   `BLE_CENTRAL_CONNECT` and 707 `BLE_CENTRAL_FAIL` through
//!   2026-09-25, `feld-pocket` 49 of 107, `t114-boot` 48 of 164, and a
//!   fallback dial that cannot connect goes into the dead-end table for
//!   two minutes (`BLE_DIAL_DEAD_END`).
//! - **A dead peer is back in the next round.** Death here is
//!   instantaneous and symmetric, and the ex-peer advertises again 5 s
//!   later at its full free-slot count. On a board the two ends learn of
//!   it up to 45 s apart (the central's supervision timeout is 4 s,
//!   `conn_params.rs`; the keepalive expiry is `LINK_TIMEOUT_MS`), and a
//!   peer that died by rebooting is not advertising at all for its boot
//!   time.
//! - **A static peer's own uptime.** The fourth kind is in the model
//!   since the section below ([`Statics`]), but it is modelled as a peer
//!   that is always there: the solar node the rig's room had is
//!   solar-powered and its sessions with `feld-t114` ended 31 times in
//!   two weeks. The harness ends them on the same [`Mortality`] draw a
//!   board-to-board session ends on, which is the population the
//!   lifetimes were measured over, and it never takes the peer out of
//!   the room for a night. A peer that vanishes for hours frees the
//!   slots it held and stops competing for others, and nothing here
//!   measures which of the two dominates.
//! - **The static peer dials boards only.** The captures place all 84
//!   links it initiated at the three boards and cannot see a link of its
//!   own to the phone (they are the boards' logs, not its). So the model
//!   does not invent one, and its own outgoing slot is therefore never
//!   spent on the phone the way a board's is.
//! - **Every advertiser is heard in every window.** One round is one
//!   complete window here. The firmware collects
//!   `SCAN_WINDOW_COLLECT_MS` (3 s) out of a 5 s cycle and can miss a board advertising on a 1-2 s
//!   cadence, while a phone advertising sub-second is heard nearly
//!   always.
//!
//! # The fourth kind in the room: the solar node (#412 steady state 2)
//!
//! 306's largest stated gap was that the rig's room had a member this
//! model had no kind for, and every reading above is a reading of a room
//! missing it. [`Statics`] is that kind, with the identity check, the
//! address class and the advertisement it carries measured off the same
//! captures rather than assumed — including the one thing 306 got wrong
//! about it: it DOES advertise a capability record with a real free-slot
//! count (`caps_record=1 free_slots=3` on 1835 of the 2450
//! `BLE_SCAN_DECISION` lines the three boards logged for it, `free_slots=2`
//! on 581), so it deficits by zero because it is empty, not because it is
//! silent. And it is one of our own boards: identity
//! `e19b2b38912698a9cba4a114fb692857` is **the solar node**, which
//! periculum's own scenario declares (`hardware/ble_mesh_formation_solar.
//! toml`: `solarnode:e19b2b38`).
//!
//! ## The rig's room, with and without it
//!
//! Three boards and one phone, per 1000 orders. `stat%` is the share of
//! the TOP board's freed-slot dials that went to the static peer and
//! `top%` the share that went to the phone; `sb` counts links formed to
//! the static peer. `none` is 306's room, and every cell of it is pinned
//! as an equality here.
//!
//! | room | policy             | life     | freed% | stat%  | top%   | sb   | bb    | d/use | disc |
//! |------|--------------------|----------|-------:|-------:|-------:|-----:|------:|------:|-----:|
//! | none | eager/mostfree     | immortal | 100.00 |   0.00 | 100.00 |    0 |  2040 | 2.04  |    0 |
//! | none | eager/mostfree     | 45 s     |   5.81 |   0.00 | 100.00 |    0 | 26790 | 1.40  |    0 |
//! | none | eager/mostfree     | 600 s    |  56.90 |   0.00 | 100.00 |    0 |  3904 | 1.84  |    0 |
//! | none | eager/rotatinglast | immortal | 100.00 |   0.00 | 100.00 |    0 |  2040 | 2.04  |    0 |
//! | none | eager/rotatinglast | 45 s     |   5.81 |   0.00 | 100.00 |    0 | 26790 | 1.40  |    0 |
//! | none | eager/rotatinglast | 600 s    |  56.90 |   0.00 | 100.00 |    0 |  3904 | 1.84  |    0 |
//! | none | eager/groupfirst   | immortal | 100.00 |   0.00 | 100.00 |    0 |  2618 | 1.31  |    0 |
//! | none | eager/groupfirst   | 45 s     |   0.06 |   0.00 |   0.41 |    0 | 31500 | 1.27  |    0 |
//! | none | eager/groupfirst   | 600 s    |  26.95 |   0.00 |  59.04 |    0 |  5238 | 1.16  |    0 |
//! | none | eager/silencefull  | 45 s     |   0.06 |   0.00 |   0.41 |    0 | 31500 | 1.27  |    0 |
//! | rig  | eager/mostfree     | immortal |  97.01 |   2.27 |  97.11 |  472 |  2090 | 1.33  |    0 |
//! | rig  | eager/mostfree     | 45 s     |   6.71 |   0.11 |  99.89 |   98 | 26734 | 1.40  |    0 |
//! | rig  | eager/mostfree     | 600 s    |  54.87 |   6.29 |  91.91 |  586 |  3960 | 1.40  |   16 |
//! | rig  | eager/rotatinglast | immortal |  98.83 |   0.00 |  98.83 |  738 |  2090 | 1.07  |    0 |
//! | rig  | eager/rotatinglast | 45 s     |   0.04 |  99.68 |   0.32 | 4640 | 26840 | 1.27  |    0 |
//! | rig  | eager/rotatinglast | 600 s    |  22.34 |  45.69 |  50.94 | 1472 |  3976 | 1.10  |   22 |
//! | rig  | eager/groupfirst   | immortal |    -   |    -   |    -   |  838 |  2162 | 1.00  |    0 |
//! | rig  | eager/groupfirst   | 45 s     |   0.00 |  99.47 |   0.00 | 4658 | 26932 | 1.27  |    0 |
//! | rig  | eager/groupfirst   | 600 s    |   0.52 |  79.10 |   1.74 | 1546 |  4138 | 1.03  |   26 |
//! | rig  | eager/silencefull  | 45 s     |   0.00 |  99.47 |   0.00 | 4658 | 26932 | 1.27  |    0 |
//!
//! (`eager/silencefull` is identical to `eager/groupfirst` in every cell
//! of every row, so only one of its rows is repeated here; the test
//! prints both and asserts the equality. Why they coincide is the second
//! finding below.)
//!
//! **306's finding does not survive the fourth kind.** In the rig's own
//! room under the shipped order the top board's freed outgoing slot goes
//! to the static peer 99.68 % of the time at the capture's short mode and
//! to the phone 0.32 % — which is the rig's own ledger after 2026-09-25,
//! when the shipped order reached the boards: `feld-t114` made 25
//! outgoing links from then on and all 25 went to the solar node. The
//! same room under the order the shipped one REPLACED gives the slot back
//! to the phone (99.89 %), which is the rig's earlier ledger (20 of
//! `feld-t114`'s 65 outgoing links went to the phone, 42 to the solar
//! node, 3 to a board). The model reproduces both eras of the capture and
//! the only thing that moves between them is the fallback order. So the
//! shipped order DOES defend the freed slot in the small room — against
//! the phone. What 306 measured as "100 % to the phone, under both
//! orders" was a three-board model of a four-board room.
//!
//! ## Ten and twenty boards, one phone and one static peer
//!
//! `sfreed%` is the room-average version of `stat%`. The immortal row is
//! left out: at these sizes every board is linked long before the horizon
//! and the freed-slot column has almost no denominator.
//!
//! | n  | policy             | life  | freed% | sfreed% | stat%  | top%   | sb   | bb     | d/use | disc |
//! |---:|--------------------|-------|-------:|--------:|-------:|-------:|-----:|-------:|------:|-----:|
//! | 10 | eager/mostfree     | 45 s  |   3.47 |    0.00 |   0.00 | 100.00 |    2 | 116870 | 1.27  |    0 |
//! | 10 | eager/mostfree     | 600 s |  36.84 |    0.00 |   0.00 | 100.00 |   12 |  17526 | 1.05  |    2 |
//! | 10 | eager/rotatinglast | 45 s  |   0.00 |    4.30 |  99.92 |   0.04 | 5810 | 116710 | 1.27  |    0 |
//! | 10 | eager/rotatinglast | 600 s |   0.88 |    7.60 |  87.19 |   8.87 | 1650 |  17610 | 1.03  |   32 |
//! | 10 | eager/groupfirst   | 45 s  |   0.00 |    4.31 |  99.88 |   0.00 | 5822 | 116726 | 1.27  |    0 |
//! | 10 | eager/groupfirst   | 600 s |   0.00 |    7.62 |  92.13 |   0.00 | 1672 |  17618 | 1.02  |   36 |
//! | 20 | eager/mostfree     | 45 s  |   1.63 |    0.00 |   0.00 | 100.00 |    6 | 236070 | 1.27  |    0 |
//! | 20 | eager/mostfree     | 600 s |  22.06 |    0.00 |   0.00 | 100.00 |   16 |  36272 | 1.04  |   12 |
//! | 20 | eager/rotatinglast | 45 s  |   0.00 |    1.96 | 100.00 |   0.00 | 5390 | 236596 | 1.27  |    0 |
//! | 20 | eager/rotatinglast | 600 s |   0.01 |    3.44 |  86.76 |   0.28 | 1614 |  36554 | 1.03  |   78 |
//! | 20 | eager/groupfirst   | 45 s  |   0.00 |    1.95 | 100.00 |   0.00 | 5386 | 236646 | 1.27  |    0 |
//! | 20 | eager/groupfirst   | 600 s |   0.00 |    3.45 |  86.52 |   0.00 | 1620 |  36550 | 1.03  |   78 |
//!
//! The three-board result is not a small-room artefact: at ten and twenty
//! boards the shipped order spends the top board's freed slot on the
//! static peer too (99.92 % and 100.00 %), and the address order still
//! spends it on the phone (100 % at both sizes). One static peer in the
//! room is enough to empty the phone's column at every size measured.
//!
//! ## Where the static peer's ADDRESS sits is the other half
//!
//! The rig's solar node held `d916e2923ed2`, below all three boards
//! (`dcb18ad99cdb` < `e1ac9eb3f05c` < `e81d77f09cdc`), and that is the
//! benign position: the strict rule hands IT every board and hands no
//! board a verdict for it, so it can only ever take a FALLBACK dial. With
//! its address drawn like a board's instead ([`Statics::drawn`]), every
//! board below it dials it STRICTLY and spends its one outgoing slot
//! there: in the three-board room the board graph then splits in 624 of
//! 1000 orders under the shipped order (against 0 with the peer at the
//! rig's address and 0 without the peer at all), and `bb` falls from 2040
//! to 1248. A room's fourth node is cheap or ruinous depending on where
//! its address lands, and #375's own question — the fallback class
//! spreading across boards — is not what decides that; the strict sort is.
//!
//! Read the `disc` column with the definition in mind: it counts the `n`
//! boards' own graph, and a link to the static peer counts in neither
//! `disc` nor `bb`, exactly as a link to a phone does not. On the rig the
//! solar node IS a mesh node, so those columns overstate what it costs
//! the mesh. `stat%` is the column that answers #412: where the dial
//! went.
//!
//! ## Item 3's two candidate keys, measured
//!
//! Both are harness-side policies over the SHIPPED table (no `src`
//! change): [`TargetChoice::GroupAboveDeficit`] asks the rotating group
//! above the free-slot deficit — the shipped key's two middle terms
//! swapped — and [`TargetChoice::SilenceIsFull`] leaves the key alone and
//! reads a peer that advertised no record as FULL instead of empty.
//!
//! - **In the empty room they cost nothing, measured as an equality.** At
//!   ten and twenty boards, `disc` 0, boardless 0, `bb` 10 000 and
//!   20 000, `d/use` 1.00 — the same cells as the shipped order, dial for
//!   dial. So neither is the deviation from #375's guarantee (`disc` = 0
//!   at n = 20) that #375 does not allow.
//! - **In the room 306 measured — three boards and a phone, no static
//!   peer — they are what defends the freed slot.** The top board's share
//!   of it spent on the phone falls from 100.00 % to 0.41 % at the
//!   capture's short mode, `bb` RISES from 26 790 to 31 500 (+17.6 %) and
//!   `d/use` falls from 1.40 to 1.27. At the 600 s row, from 100.00 % to
//!   59.04 %; at `immortal`, neither can do anything, and the reason is
//!   in the `solo%` column — 100 % of those windows have no board on
//!   offer at all, so there is nothing to prefer.
//! - **With the static peer present they add almost nothing at the short
//!   mode** (99.68 % of the top board's freed slots already went to the
//!   static peer) **and they close the residual at the tail**: 8.87 % ->
//!   0.00 % at n = 10 and 0.28 % -> 0.00 % at n = 20 on the 600 s row,
//!   and 50.94 % -> 1.74 % in the rig's three-board room.
//! - **This instrument cannot choose between the two.** Every cell of
//!   every row is identical, and that is a fact about the ROOM: the only
//!   peer in it with no capability record is the phone, and the only peer
//!   with a rotating address is the phone, so both keys demote the same
//!   single peer. They come apart only for a peer in one set and not the
//!   other — a rotating peer that advertises a record (no Columba does
//!   today), or a static peer that advertises none (an older board) — and
//!   `the_two_candidate_keys_are_two_different_policies` is that pair of
//!   windows, through the real table.
//!
//! ## The sentence for #412
//!
//! With the rig's fourth kind in the room, the shipped fallback order
//! already defends the freed slot: the phone takes 0.32 % of the top
//! board's freed slots in the rig's own three-board room, 0.04 % at ten
//! boards and 0.00 % at twenty, and the model reproduces the capture's
//! two eras on either side of 2026-09-25 with nothing but the order
//! changing.
//!
//! **Weakened 2026-09-27 (#412, [`ConnectFailure`]), and the mechanism is
//! an asymmetry this instrument could not see.** 0.32 % is measured under
//! "every dial connects". With the captures' own rates on, the phone
//! takes **11.60 %** of that slot in the same room and the same cell —
//! thirty-six times more — because the peer the shipped order PREFERS
//! carries a static address and a third of the dials to it fail, while
//! the peer it DEMOTES carries a rotating one and, in the population that
//! reaches a Reticulum service at all, never fails. The order is a
//! preference and a failed dial is a retry, so the demoted peer collects
//! the retries. The defence is still real (11.60 % against the 99.89 %
//! the order it replaced gives) — it is not the near-exclusion 0.32 %
//! reads as. The rig's own post-2026-09-25 ledger, 25 of 25 to the solar
//! node, is reproduced by `off` (0.32 %) and by
//! [`ConnectFailure::CAPTURE_STRANGERS`] (0.40 %) and NOT by
//! [`ConnectFailure::CAPTURE`] — which is the row saying the rig's room
//! since 2026-09-22 is the stranger room, where the phone's own dials
//! fail too. 306's "the shipped order does not defend that slot" was an
//! artefact of a three-board model of a four-board room, and the tier
//! from part 1 is still worth exactly zero (0 of the offers in every cell
//! here reach `DialPreference::CannotDialUs` — the solar node advertises
//! `caps=0x1e`/`0x1c`, bit 0 clear). What remains open is the room with NO
//! such peer, and there both of item 3's candidates defend it — 100.00 %
//! to 0.41 % at the capture's short mode — at zero cost in `disc` and
//! `d/use` in the empty room, so neither is a deviation #375 forbids;
//! both also hand back 17.6 % more board-to-board links in that room than
//! the shipped order. Neither candidate can be chosen over the other on
//! any row this instrument can measure, because the phone is the only peer
//! that is both silent and rotating. What no candidate KEY defends is the
//! window with one candidate in it (`solo%` = 100 % on the immortal rows,
//! 57 % of the rig room's churn dials when links stand), and a ledger is
//! the only thing that can refuse a dial the window has nobody to prefer
//! over — which is the section below.
//!
//! # The dial ledger: what a board remembers (#412 part 2)
//!
//! Every policy above ORDERS the peers a window heard, and 311's own
//! closing sentence is that an order needs something to prefer: `solo%`
//! is 100 % of the top board's churn dials on the immortal rows, 57 % of
//! the rig room's when links stand and 38 % at the 600 s mean. Part 2 is
//! the parameter for that window — [`Ledger`], keyed by IDENTITY, fed by
//! every dial outcome including the post-connect duplicate refusal, and
//! consulted before a fallback dial. The rule itself is not here: it is
//! [`leviculum_ble_tx::DialLedger`], and [`Ledger::policy`] is the whole
//! of what this file says about it, so the tables below measure the
//! shipped rule rather than a restatement of it.
//!
//! `hold` counts the windows the pause held shut where the board would
//! otherwise have spent a fallback dial, `hold%` the share of those that
//! had no board on offer at all — the solo window — and `refused` the
//! dials spent to be told the identity was already live. The room is the
//! rig's, three boards and a phone, under the shipped order; `none` is
//! without the solar node and `rig` with it.
//!
//! | room | ledger | life     | hold  | hold% | dials | refused | bb    | d/use | disc |
//! |------|--------|----------|------:|------:|------:|--------:|------:|------:|-----:|
//! | none | off    | immortal |     0 |   -   |  9556 |    4746 |  2040 | 2.04  |    0 |
//! | none | k=2    | immortal |  8410 | 24.54 |  7402 |    2248 |  2040 | 1.47  |    0 |
//! | none | k=3    | immortal |  6526 | 15.81 |  7686 |    2704 |  2040 | 1.59  |    0 |
//! | none | k=5    | immortal |  5674 | 18.19 |  7970 |    2988 |  2040 | 1.64  |    0 |
//! | none | off    | 45 s     |     0 |   -   | 32258 |    3570 | 26790 | 1.40  |    0 |
//! | none | k=3    | 45 s     |  3160 |  0.00 | 30960 |    2272 | 26790 | 1.34  |    0 |
//! | none | off    | 600 s    |     0 |   -   | 11574 |    5058 |  3904 | 1.84  |    0 |
//! | none | k=3    | 600 s    |  7104 |  7.60 |  9314 |    2706 |  3904 | 1.46  |    0 |
//! | rig  | off    | immortal |     0 |   -   |  4220 |     204 |  2090 | 1.07  |    0 |
//! | rig  | k=3    | immortal |     0 |   -   |  4220 |     204 |  2090 | 1.07  |    0 |
//! | rig  | off    | 45 s     |     0 |   -   | 31594 |      32 | 26840 | 1.27  |    0 |
//! | rig  | k=3    | 45 s     |    58 |  0.00 | 31588 |      32 | 26852 | 1.27  |    0 |
//! | rig  | off    | 600 s    |     0 |   -   |  6670 |     424 |  3976 | 1.10  |   22 |
//! | rig  | k=3    | 600 s    |   482 |  2.07 |  6570 |     326 |  3964 | 1.08  |   22 |
//!
//! ## It defends the window, and it costs that room nothing
//!
//! In the room 311 left open — three boards and a phone, no other node —
//! the ledger refuses about one solo window per arrival order (1032 of
//! the 6526 windows it holds shut at `k=3`, immortal) and takes the
//! room's dials from 9556 to 7686, its refused dials from 4746 to 2704
//! and its dials per link that lasted from 2.04 to 1.59. The
//! board-to-board links formed and the split-graph count do not move by
//! one: **2040 and 0 at every K in {2, 3, 5} and at every lifetime**,
//! asserted as equalities. Every board link in that room is won on a
//! strict verdict, and the ledger never touches a strict verdict.
//!
//! What it does NOT do is send the dial somewhere better. In the solo
//! window there is nowhere better — `freed%` and `top%` stay at 100 %,
//! because the dials that still happen still have only the phone to go
//! to. The slot stays free instead of being spent, which is the only
//! defence that window admits, and the column that shows it is `dials`.
//!
//! ## In the rig's real room it is nearly inert, which is the right answer
//!
//! With the solar node present the shipped order already sends the freed
//! slot to a peer whose sessions last, so there is no run of waste to
//! remember: at the capture's short mode the ledger holds 58 windows of
//! the room's 31 588 dials shut and board-to-board links go UP by 12. At
//! the 600 s row it holds 482 and costs 12 of 3976 links (0.3 %) at
//! `disc` unchanged. The bound the test asserts is 1 % of the room's
//! links and `disc` no worse than the shipped row, at every K.
//!
//! ## Ten and twenty boards: measurably inert
//!
//! At ten boards the ledger holds at most 1324 windows of a thousand
//! orders shut and `disc` and the stranded-board count are IDENTICAL to
//! the shipped row in every room, at every lifetime, for every K. At
//! twenty the phone room is inert to the last dial — that room makes no
//! refused dial at all over the thousand orders, so no K has anything to
//! remember. A board in a room of ten has strict candidates and rarely
//! reaches a fallback dial; the ledger is a small-room mechanism, and a
//! small room is what a mesh of boards in one flat is.
//!
//! In a room of boards whose links never end it cannot fire at all, by
//! construction rather than by seed: no dial ever has a wasted outcome,
//! so #375's own guarantee cell (`disc` = 0, `bb` = 10 000 and 20 000,
//! `d/use` = 1.00) is bit-identical with the ledger on. That is asserted
//! as an equality for every K.
//!
//! ## The two parameters that are not K, measured
//!
//! **The pause is the knob, and one scan cycle is too short by
//! construction.** A teardown resets the strict clock, so a board spends
//! one full scan cycle ([`SCAN_FALLBACK_AFTER_MS`], 30 s) in strict mode
//! before it can reach a fallback dial at all; a pause of exactly that
//! length expires in the round the fallback becomes available and holds
//! **nothing** shut — every column of the `k=3 pause=30s` row equals the
//! `off` row, which the test asserts. The tables are measured at 120 s,
//! the period the firmware's address table already waits after a dial
//! that bought nothing ([`DEAD_END_TTL_MS`]); three minutes holds more
//! windows and starts costing `disc` in the rig room (22 -> 24).
//!
//! **T under-states the waste on purpose.** At
//! [`USEFUL_SESSION_MS`] (15 s, one keepalive interval) the ledger holds
//! 6526 windows shut in the `none` immortal room; at 30 s it holds 10 636
//! and at 45 s 11 948, with `hold%` rising from 15.8 % to 48.4 % and
//! 54.0 % — more of the pause landing on the window it is for. Both also
//! start costing: in the rig room at 600 s, `disc` 22 -> 24 and links
//! 3976 -> 3952. The generous bound is the one that moves no other
//! column.
//!
//! ## Where the immortal rows' waste comes from, and it is not sessions
//!
//! The two feeds are separable and the control separates them. With a
//! churning peer that only ADVERTISES — no duplicate refusal is possible,
//! the room's `refused` column is exactly zero — and links that never
//! end, the ledger fires **not at all**, and the reason is a phase lock:
//! the top board's cycle after a rotation is the expiry sweep
//! ([`LINK_EXPIRY_ROUNDS`], 9 rounds) plus the strict phase
//! ([`FALLBACK_AFTER_ROUNDS`], 6), so it re-dials 6 rounds into the
//! peer's 9-round rotation and every session it buys is exactly 3 rounds
//! — exactly [`USEFUL_SESSION_MS`], which belongs to the useful side. So
//! on the immortal rows every wasted dial the ledger sees is a REFUSAL,
//! and what it removes there is the duplicate-refusal storm. Turn
//! mortality on in the same room and the session-length feed appears on
//! its own, with `refused` still zero.
//!
//! ## What part 3 wired, and what part 2 still does not answer
//!
//! The firmware reads [`leviculum_ble_tx::DialLedger`] since #412 part 3
//! (`leviculum-nrf/src/ble/columba.rs:1768`, the board-global instance).
//! Three outcomes feed it — the two duplicate refusals that used to write
//! only the 120 s address skip (`columba.rs:878` incoming, `:2233`
//! outgoing) and the central teardown's session length (`:2265`) — and
//! one site reads it, the fallback branch of the scanner's eligibility
//! filter (`:2061`).
//!
//! The dial that never CONNECTS still does not feed it, and that is a
//! decision rather than an omission. It has no identity — nothing
//! connected — so the only table it could join is one keyed by address,
//! and the firmware already has that table: a fallback dial that cannot
//! connect condemns its address for `DEAD_END_TTL`, 120 s, against a
//! rotation period the captures put at 48 s ([`CHURN_ROTATE_MS`]). A
//! per-address run of failed connects would therefore need a second
//! failure at an address the first failure has already made unreachable
//! for longer than the address lives. It cannot reach two, let alone
//! [`LedgerPolicy::WASTED_RUN`]. Measuring it here would mean teaching
//! this harness that a dial can fail to connect, which is the first of
//! the four limits under "What this harness still cannot see" and is
//! load-bearing for every table above: every one of them is measured
//! under "every dial connects", so the change is a re-measurement of the
//! document, not of a column.
//!
//! # The dial that fails to connect (#412, the instrument's first limit)
//!
//! [`ConnectFailure`] is that re-measurement, and its own docs carry the
//! captures it is read off, the two shapes the failure has and what the
//! firmware does with each stage. `ConnectFailure::NONE` reproduces every
//! row above bit for bit — not as a claim but as the 23 tests that were
//! here before this section, every pinned cell of them unchanged.
//!
//! ## What the captures say about the SHAPE, before any model
//!
//! 3034 dials, paired `BLE_CENTRAL_CONNECT addr=A` to the
//! `BLE_CENTRAL_FAIL addr=A` or `BLE_CENTRAL_UP` that follows it, over
//! the three `ble-drop` logs, 2026-09-12 to 2026-09-27. The doc's own
//! 758/707 for `feld-t114` is this data through 2026-09-25, exactly.
//!
//! **Per board and peer kind** (`fail%` is failures per dial,
//! `conn%` the share of those failures at `stage=connect` — the only
//! stage that condemns anything):
//!
//! | board | peer kind | dials | fail% | conn% | cost s |
//! |-------|-----------|------:|------:|------:|-------:|
//! | `feld-t114`   | rotating (RPA)  | 2675 | 99.3 |  7.5 | 2.28 |
//! | `feld-t114`   | solar node      |   53 | 41.5 | 31.8 | 6.11 |
//! | `feld-t114`   | board           |    4 | 25.0 |100.0 | 3.26 |
//! | `t114-boot`   | board           |  134 | 31.3 | 35.7 | 6.01 |
//! | `t114-boot`   | rotating (RPA)  |   23 | 26.1 | 33.3 | 3.46 |
//! | `feld-pocket` | board           |  122 | 40.2 | 32.7 | 8.01 |
//!
//! ```text
//! grep -E 'BLE_CENTRAL_(CONNECT|FAIL|UP)|BLE_DIAL_DEAD_END' <board>.log
//! ```
//!
//! **The rotating column is not one population, and that is the finding
//! the model turns on.** Split `feld-t114`'s 450 rotating addresses by
//! whether any dial to them ever reached a Reticulum service:
//!
//! | population | addrs | dials | fail% | dials spent per address |
//! |------------|------:|------:|------:|------------------------:|
//! | reached the service | 20 | 20 | 0.0 | 1 |
//! | never reached it | 430 | 2655 | 100.0 | med 6, max 13 |
//!
//! Not one address is mixed — and the same holds on `t114-boot` (17 of
//! 17 and 6 of 6) and `feld-pocket`. So a rotating peer does not fail per
//! dial: the ADDRESS is reachable or it is not, and the draw belongs to
//! the rotation. The never-reached population fails
//! `stage=discover err=ServiceNotFound`, which writes no dead end at any
//! stage, which is why one dead address costs six dials instead of one.
//!
//! **Clustering, for the static kind, is mild and is not modelled:**
//! `P(fail | the last dial to this address failed)` is 0.48 to 0.69
//! against a base of 0.31 to 0.40. The draw here is memoryless, which
//! under-states the runs.
//!
//! **What a failed dial costs** is not the failure but the gap to the
//! board's next `BLE_CENTRAL_CONNECT`: median 16.35 s after a
//! connect-stage failure and 11.56 s after a post-connect one, against
//! [`ROUND_MS`] = 5 s. Three rounds and two, and the board is neither
//! scanning nor linked in them.
//!
//! **One era note, because the doc's 93 % averages two rooms.** Through
//! 2026-09-16 a rotating-address dial from `feld-t114` mostly worked (3
//! of 23 failed). From 2026-09-22 not one of 2652 did. The static kind
//! did not move across that boundary (31-44 % in both), which is what
//! says the change is in the room and not in the board.
//!
//! ## 306's and 311's rows, re-measured
//!
//! The rig's room, three boards and one dialling phone, `none` without
//! the solar node and `rig` with it, per 1000 orders. `off` is the row as
//! the sections above measured it.
//!
//! | room | policy             | life     | failure   | fail%  | top%   | sb   | bb    | d/use | disc |
//! |------|--------------------|----------|-----------|-------:|-------:|-----:|------:|------:|-----:|
//! | none | eager/rotatinglast | immortal | off       |   0.00 | 100.00 |    0 |  2040 | 2.04  |    0 |
//! | none | eager/rotatinglast | immortal | capture   |   9.96 | 100.00 |    0 |  2032 | 2.41  |    0 |
//! | none | eager/rotatinglast | immortal | strangers |  82.95 | 100.00 |    0 |  2032 | 6.47  |    0 |
//! | none | eager/rotatinglast | 45 s     | off       |   0.00 | 100.00 |    0 | 26790 | 1.40  |    0 |
//! | none | eager/rotatinglast | 45 s     | capture   |  31.32 | 100.00 |    0 | 23350 | 2.04  |  238 |
//! | none | eager/rotatinglast | 45 s     | strangers |  44.43 | 100.00 |    0 | 23422 | 2.28  |  210 |
//! | none | eager/rotatinglast | 600 s    | off       |   0.00 | 100.00 |    0 |  3904 | 1.84  |    0 |
//! | none | eager/rotatinglast | 600 s    | capture   |  15.35 | 100.00 |    0 |  3850 | 2.23  |   22 |
//! | none | eager/rotatinglast | 600 s    | strangers |  75.04 | 100.00 |    0 |  3846 | 4.36  |   28 |
//! | rig  | eager/rotatinglast | immortal | off       |   0.00 |  98.83 |  738 |  2090 | 1.07  |    0 |
//! | rig  | eager/rotatinglast | immortal | capture   |  25.17 |  94.38 |  674 |  2140 | 1.50  |    4 |
//! | rig  | eager/rotatinglast | immortal | strangers |  62.66 |  53.85 |  726 |  2056 | 2.71  |   58 |
//! | rig  | eager/rotatinglast | 45 s     | off       |   0.00 |   0.32 | 4640 | 26840 | 1.27  |    0 |
//! | rig  | eager/rotatinglast | 45 s     | capture   |  34.98 |  11.60 | 2876 | 23370 | 1.97  |  230 |
//! | rig  | eager/rotatinglast | 45 s     | strangers |  36.39 |   0.40 | 2962 | 23294 | 2.00  |  258 |
//! | rig  | eager/rotatinglast | 600 s    | off       |   0.00 |  50.94 | 1472 |  3976 | 1.10  |   22 |
//! | rig  | eager/rotatinglast | 600 s    | capture   |  29.68 |  57.32 | 1306 |  3988 | 1.63  |   28 |
//! | rig  | eager/rotatinglast | 600 s    | strangers |  50.12 |   4.55 | 1356 |  3894 | 2.08  |   42 |
//! | rig  | eager/mostfree     | 45 s     | off       |   0.00 |  99.89 |   98 | 26734 | 1.40  |    0 |
//! | rig  | eager/mostfree     | 45 s     | capture   |  31.13 |  99.73 |   44 | 23204 | 2.03  |  200 |
//! | rig  | eager/mostfree     | 45 s     | strangers |  44.43 |  92.31 |   60 | 23372 | 2.29  |  222 |
//!
//! (`eager/mostfree` is identical to `eager/rotatinglast` in every cell
//! of the `none` room, at every level, as it is at `off`; the test prints
//! both.)
//!
//! ## #375's guarantee cell, re-measured
//!
//! Ten and twenty boards with nothing else in the room, and the same room
//! with one phone, per 1000 orders. `bdls` counts boards left with no
//! board link. The empty room holds nothing rotating, so `capture` and
//! `strangers` are equal in every cell — asserted, and the model's own
//! positive control that the rotating half is the only thing between the
//! two levels.
//!
//! | n  | room  | policy             | life     | failure   | fail% | top%   | bb     | d/use | disc | bdls |
//! |---:|-------|--------------------|----------|-----------|------:|-------:|-------:|------:|-----:|-----:|
//! | 10 | empty | eager/rotatinglast | immortal | off       |  0.00 |    -   |  10000 | 1.00  |    0 |    0 |
//! | 10 | empty | eager/rotatinglast | immortal | capture   | 35.63 |    -   |  10000 | 1.55  |    0 |    0 |
//! | 10 | empty | eager/rotatinglast | 45 s     | off       |  0.00 |   0.00 | 122812 | 1.27  |    0 |    0 |
//! | 10 | empty | eager/rotatinglast | 45 s     | capture   | 35.16 |   0.00 | 106730 | 1.96  |  508 |  280 |
//! | 20 | empty | eager/rotatinglast | immortal | off       |  0.00 |    -   |  20000 | 1.00  |    0 |    0 |
//! | 20 | empty | eager/rotatinglast | immortal | capture   | 35.54 |    -   |  20000 | 1.55  |   14 |    0 |
//! | 20 | empty | eager/rotatinglast | 45 s     | off       |  0.00 |   0.00 | 242126 | 1.27  |    0 |    0 |
//! | 20 | empty | eager/rotatinglast | 45 s     | capture   | 34.97 |   0.00 | 211198 | 1.95  |  820 |  416 |
//! | 10 | phone | eager/rotatinglast | 45 s     | off       |  0.00 |  79.29 | 118040 | 1.27  |    0 |    0 |
//! | 10 | phone | eager/rotatinglast | 45 s     | capture   | 34.56 |  68.98 | 103788 | 1.94  |  682 |  394 |
//! | 10 | phone | eager/rotatinglast | 45 s     | strangers | 36.67 |  10.08 | 104488 | 2.00  |  636 |  362 |
//! | 20 | phone | eager/rotatinglast | 45 s     | off       |  0.00 |  51.03 | 239166 | 1.27  |    0 |    0 |
//! | 20 | phone | eager/rotatinglast | 45 s     | capture   | 34.91 |  21.15 | 210254 | 1.95  |  882 |  530 |
//! | 20 | phone | eager/rotatinglast | 45 s     | strangers | 35.12 |   1.14 | 210560 | 1.96  |  874 |  520 |
//! | 10 | phone | eager/mostfree     | 45 s     | capture   | 34.17 | 100.00 | 102548 | 1.93  |  688 |  404 |
//! | 20 | phone | eager/mostfree     | 45 s     | capture   | 34.50 | 100.00 | 206948 | 1.93  |  902 |  578 |
//!
//! **The formation phase survives and the steady state does not.** With
//! nothing else moving in the room, a dial that failed is a dial the
//! board makes again: all 10 000 links still form, no order splits, and
//! the whole cost is `d/use` going from 1.00 to 1.55. At twenty boards
//! the split count stops being exactly zero (14 of 1000), which is enough
//! to say that #375's `disc` = 0 is a cell measured under "every dial
//! connects" rather than a property of the rule. With links that end it
//! is not close: 508 and 820 split orders, 280 and 416 boards per 1000
//! orders holding no board link at the snapshot, 13 % fewer links formed.
//! That is the largest single thing this parameter changes in the
//! document, and it is in a column nobody was watching — the churn
//! columns barely move.
//!
//! ## 321's ledger rows, re-measured
//!
//! Three boards and a phone, shipped order, per 1000 orders.
//!
//! | room | ledger | life     | failure   | hold | hold% | dials | refused | bb    | d/use | disc |
//! |------|--------|----------|-----------|-----:|------:|------:|--------:|------:|------:|-----:|
//! | none | off    | immortal | off       |    0 |   -   |  9556 |    4746 |  2040 | 2.04  |    0 |
//! | none | k=3    | immortal | off       | 6526 | 15.81 |  7686 |    2704 |  2040 | 1.59  |    0 |
//! | none | k=3    | immortal | capture   | 7336 |  9.00 |  8510 |    2724 |  2032 | 1.86  |    0 |
//! | none | k=3    | immortal | strangers |   24 |  0.00 | 14618 |     230 |  2032 | 6.47  |    0 |
//! | none | k=3    | 45 s     | off       | 3160 |  0.00 | 30960 |    2272 | 26790 | 1.34  |    0 |
//! | none | k=3    | 45 s     | capture   | 2602 |  0.00 | 40168 |    2136 | 23350 | 1.99  |  238 |
//! | none | k=3    | 600 s    | off       | 7104 |  7.60 |  9314 |    2706 |  3904 | 1.46  |    0 |
//! | none | k=3    | 600 s    | capture   | 7448 |  5.72 | 11178 |    2714 |  3850 | 1.82  |   22 |
//! | rig  | k=3    | immortal | capture   |  328 |  0.00 |  6094 |     402 |  2136 | 1.49  |    6 |
//! | rig  | k=3    | 45 s     | off       |   58 |  0.00 | 31588 |      32 | 26852 | 1.27  |    0 |
//! | rig  | k=3    | 45 s     | capture   |   56 |  0.00 | 41150 |     262 | 23368 | 1.97  |  228 |
//! | rig  | k=3    | 600 s    | off       |  482 |  2.07 |  6570 |     326 |  3964 | 1.08  |   22 |
//! | rig  | k=3    | 600 s    | capture   |  638 |  9.40 |  9706 |     546 |  3966 | 1.61  |   32 |
//!
//! **321's conclusion holds, in both directions.** The ledger still costs
//! the phone room not one board-to-board link at any level — 2032 and
//! 23 350 and 3850 with the ledger and without it, equalities — and stays
//! inside 321's own 1 % bound in the rig room. And the instrument's limit
//! was UNDER-stating its work rather than over-stating it: at the capture
//! rates the pause holds 7336 windows shut on the immortal row against
//! 6526, because a room whose dials fail reaches more fallback windows.
//!
//! **The stranger level says where the immortal rows' waste came from,
//! from the other side.** 321 found it was the duplicate refusal and not
//! the session length, by a phase-lock argument. Make the phone
//! unreachable and the refusals go with it — 2704 to 230 — and the ledger
//! falls from 6526 held windows to 24. A refusal needs a live link to
//! refuse against; a phone no dial can reach holds none. Read the row
//! carefully, though, and this is the fourth control: the room did not
//! get cheaper, it got emptier. Dials go from 7686 to 14 618 and `d/use`
//! from 1.59 to 6.47.
//!
//! ## Does the model still reproduce the rig's 100 %? Closer in one room
//! ## and further in the other
//!
//! In the room 306 read the rig's 7-of-7 in — three boards, one phone, no
//! solar node — the top board's freed outgoing slot goes to the phone
//! **100.00 %** of the time at every failure level and every lifetime,
//! `off` included. The instrument's first limit was not what put that
//! column at 100 %, and closing it moves the answer by nothing at all.
//!
//! In the four-node room the model moves AWAY from the rig, and that is
//! the honest reading rather than a tuning problem. The rig's ledger
//! after 2026-09-25 is 25 of 25 to the solar node, 0 to the phone; the
//! model gives 0.32 % at `off`, 0.40 % at `strangers` and 11.60 % at
//! `capture`. Under `capture` the expected count is ~3 of 25 and the
//! observed is 0, which a 25-draw binomial puts at about 4.5 % — tension,
//! not agreement. The reading the two rows agree on is that the rig's
//! room SINCE 2026-09-22 is the stranger room: 2652 of 2652 of
//! `feld-t114`'s rotating-address dials failed in it, and that is exactly
//! the room where the model puts the phone back at 0.40 %.
//!
//! ## What this harness still cannot see, with one fewer entry
//!
//! The four bullets above lose their first. What the closing added
//! instead, stated rather than ranked:
//!
//! - **The peers' own dials never fail.** The churning peer's and the
//!   static peer's dials are measured off the BOARDS' logs, which see a
//!   peer's successful dial arrive as a link and cannot see its failed
//!   one at all.
//! - **The static kind's stickiness is dropped.** `P(fail | last failed)`
//!   is 0.48 to 0.69 against a base of 0.31 to 0.40 and the draw here is
//!   memoryless, so the model under-states the runs.
//! - **A stranger is modelled as the phone being unreachable**, not as a
//!   fifth kind beside it. The rig's room from 2026-09-22 had 287
//!   rotating addresses in two days that no dial could get a service out
//!   of; whether any of them was the Columba is not something the boards'
//!   logs can answer, because a failed dial never reads an identity.

use leviculum_ble_tx::{
    dial_preference, free_slots, judge_duplicate, should_initiate, with_free_slots, CandidateTable,
    ConnectDecision, DialLedger, DialPreference, DupVerdict, FallbackOrder, LedgerPolicy, Origin,
    ScanMode, CAP_PERIPHERAL_ONLY, LINK_ABANDONED_MS, LINK_TIMEOUT_MS, MIN_USABLE_MTU,
    SCAN_FALLBACK_AFTER_MS, WINDOW_CANDIDATES,
};

/// The firmware's incoming-slot count (`PERIPH_LINKS`, #372), taken
/// from the shared constant the advertised record is bounded by rather
/// than restated — since #375 item 3 the two are one fact.
const PERIPH_SLOTS: usize = leviculum_ble_tx::PERIPH_SLOTS as usize;

/// The fallback bound, in scan rounds. The firmware bounds the strict
/// search in time (`SCAN_FALLBACK_AFTER_MS` = 30 s over 5 s retry
/// cycles, so about six passes); one sim round is one scan pass, hence
/// six empty rounds before a board's mode flips. The lock rate barely
/// moves with this bound (checked from 6 to 50 rounds), so its exact
/// value is not what the assertions lean on.
const FALLBACK_AFTER_ROUNDS: u32 = 6;

/// Seeded orders per claim. The strict rule's failure rate at 10 boards
/// is about one order in five, so a thousand orders leaves a vanishing
/// chance of the control finding nothing.
const ORDERS: u64 = 1_000;

/// What one round is worth in wall-clock milliseconds (#412).
///
/// Before the churning peer the harness had no clock at all: nothing in
/// it ended, so a round was just "one scan pass". A peer that rotates
/// its address every N *seconds* forces the conversion, and the
/// conversion is already fixed by the two constants above — the
/// firmware reaches fallback after [`SCAN_FALLBACK_AFTER_MS`], the
/// harness after [`FALLBACK_AFTER_ROUNDS`] rounds, so a round is 5 s.
/// That is also the firmware's own search-connect-backoff cycle: one
/// connect timeout (`CONNECT_TIMEOUT_10MS`, 5 s) or one retry backoff
/// (`CENTRAL_RETRY_BACKOFF_MS`, 5 s) per pass.
const ROUND_MS: u64 = SCAN_FALLBACK_AFTER_MS / FALLBACK_AFTER_ROUNDS as u64;

/// A duration in rounds, rounded down — every period below is stated
/// in the milliseconds its source states it in, never in rounds.
const fn rounds(ms: u64) -> u32 {
    (ms / ROUND_MS) as u32
}

/// How often the churning peer draws a new address (#412).
///
/// The measurement is the room capture of 2026-09-14/15: the host saw
/// the same Android Columba come up under **five addresses in four
/// minutes**, so 48 s per address — nine rounds.
const CHURN_ROTATE_MS: u64 = 4 * 60_000 / 5;

/// How long a link survives its peer going silent: the registry's own
/// expiry bound. The capture's `reason="timeout"` on the older link is
/// this bound elapsing after the rotation that abandoned it.
const LINK_EXPIRY_ROUNDS: u32 = rounds(LINK_TIMEOUT_MS);

/// The shortest session that counts as a useful link (#412 number 3).
///
/// One keepalive interval (`LINK_ABANDONED_MS` is two of them): a link
/// that did not outlive one keepalive never proved it was carrying
/// anything. Deliberately the most generous bound available — a longer
/// one would count more of the churn sessions as spent, so this choice
/// *under*-states the waste rather than manufacturing it.
const USEFUL_SESSION_MS: u64 = LINK_ABANDONED_MS / 2;

/// The firmware's `DEAD_END_TTL`: how long an address is skipped after
/// a duplicate refusal or a fallback dial that could not connect.
const DEAD_END_TTL_MS: u64 = 120_000;

/// The firmware's `DEAD_END_SLOTS` (`2 * MAX_LINKS`). Overflow reuses
/// the oldest entry — an early re-dial, not a loss.
const DEAD_END_SLOTS: usize = 8;

/// How many boards one churning peer holds links to as a central at a
/// time. A model parameter, not a protocol constant: the corpus night
/// shows the phone connected to all three boards in the room, and
/// nothing in the capture bounds it further.
const CHURN_CENTRAL_LINKS: usize = 3;

/// How long an order that never becomes quiescent is replayed (#412):
/// one with a churning peer in it, or one with link mortality on.
///
/// Ten simulated minutes. The board graph itself settles inside the
/// first `n + FALLBACK_AFTER_ROUNDS` rounds, so the horizon is not
/// about convergence; it is about seeing enough rotations for the
/// dial ledger to mean something — thirteen of them, against the five
/// the room capture covers — and, since the steady-state section, enough
/// link deaths per board for the freed slot to be spent many times
/// (about ten at [`Mortality::CAPTURE_SHORT_MODE`]). With neither churn
/// nor mortality the harness keeps its original quiescence break
/// instead, which is what makes those columns bit-identical to the
/// pre-#412 numbers.
const HORIZON_ROUNDS: u32 = rounds(10 * 60_000);

/// How a board-to-board link ends (#412 steady state) — a parameter of
/// the same kind as [`Churn`]: [`Mortality::IMMORTAL`] must reproduce
/// every row measured before it, bit for bit.
///
/// # What the lifetime stands for on a board
///
/// One drawn lifetime stands for the whole population of ways a
/// board-to-board session ends, because the captures do not separate
/// them: the peer stopped answering and the registry swept the link at
/// `LINK_TIMEOUT_MS` (`BLE_LINK_EXPIRE role=central silence_ms=45000`),
/// the controller gave up first at the supervision timeout
/// (`conn_params.rs`: 4 s in the central role), the peer rebooted, or
/// it walked out of range. What the board sees in every one of those
/// cases is the same event pair — `BLE_CENTRAL_DOWN` and
/// `BLE_CARRIER_DROP` — and the same consequence, which is the one this
/// section is about: the one outgoing slot is free again and the board
/// is back in the scan.
///
/// Both ends die at once, because a BLE disconnect is symmetric: the
/// central's outgoing slot and the peripheral's incoming slot free in
/// the same round, and both boards reset their strict clock exactly as
/// the expiry sweep above already does (`conn_link_down` ->
/// `note_strict_reset`).
///
/// # Where the distribution comes from
///
/// Measured, on the rig's own `ble-drop` captures
/// (`/home/lew/rig-run/ble-drop/{feld-t114,t114-boot,feld-pocket}.log`,
/// 2026-09-12 to 2026-09-26): a lifetime is `BLE_CENTRAL_UP peer=P` to
/// the next `BLE_CENTRAL_DOWN peer=P` on the same board's own clock
/// (`t=`, ms since boot). Pairs that cross a reboot, and UPs with no
/// DOWN, are dropped rather than counted short (88 of the 259 UPs, with
/// 7 more still open when the capture ends, leaving 164 pairs) because a
/// truncated lifetime biases the distribution downward exactly where the
/// long-lived links are. The three boards'
/// identities are `b2a8bea1` (feld-t114), `2fe95060` (t114-boot) and
/// `1d48253f` (feld-pocket); `b99af2ec` is the phone, which is what
/// nine addresses under one identity means.
///
/// Board-to-board, n = 122:
///
/// | min | p10 | p25 | med | p75 | p90 | max | mean |
/// |----:|----:|----:|----:|----:|----:|----:|-----:|
/// | 0.0 s | 12.1 s | 28.7 s | **37.2 s** | 54.7 s | 188.8 s | 14495 s | 641.8 s |
///
/// It does not give ONE distribution, and that is stated rather than
/// fitted: 72 of the 122 lifetimes (59 %) fall in [20 s, 50 s), a mode
/// sitting just under the registry's own 45 s bound, while 11 (9 %)
/// exceed ten minutes and five exceed an hour. mean/median is 17, where
/// an exponential's is 1.44, so no single-parameter fit describes both
/// modes and one would misstate whichever it did not fit. The two modes
/// are therefore two ROWS, each a stated constant:
/// [`Mortality::CAPTURE_SHORT_MODE`] and [`Mortality::CAPTURE_TAIL`].
///
/// Within a row the draw is memoryless (geometric in rounds, mean
/// `mean_ms`), which is the null shape to assume when the capture gives
/// none: a link the model has held for an hour is no likelier to die in
/// the next round than a fresh one. Two known limits of the population,
/// both of which point the same way and are not corrected for: the
/// captures span two weeks of rig work with 61 to 81 board reboots per
/// log, so part of the short mode is the rig's own reflash cadence
/// rather than steady-state mesh behaviour; and a round is
/// [`ROUND_MS`], so the 10 lifetimes under 5 s (8 %) are below the
/// harness's resolution and are drawn as one round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Mortality {
    /// Mean board-to-board link lifetime in milliseconds; 0 is a link
    /// that never ends — the pre-steady-state simulation.
    mean_ms: u64,
}

impl Mortality {
    /// A board-to-board link that never ends: every row measured
    /// before the steady-state section, and the FORMATION-phase cost
    /// those rows report.
    const IMMORTAL: Self = Self { mean_ms: 0 };

    /// The capture's short mode, stated as the firmware bound it sits
    /// under: `LINK_TIMEOUT_MS`, 45 s. The measured interquartile range
    /// is 28.7 s to 54.7 s and the median 37.2 s, so the bound is inside
    /// the range, two rounds above the median, and it is a real quantity
    /// of the stack rather than a fitted one.
    const CAPTURE_SHORT_MODE: Self = Self {
        mean_ms: LINK_TIMEOUT_MS,
    };

    /// The capture's tail, stated as the ten minutes the horizon is:
    /// p90 is 188.8 s and 9 % of the sample exceeds 600 s, so a mean of
    /// 600 s puts most of a run's links beyond the horizon and reports
    /// what a room of mostly-standing links does with the few that end.
    const CAPTURE_TAIL: Self = Self { mean_ms: 600_000 };

    /// Whether any board-to-board link ends in this configuration. When
    /// false no draw is taken from the mortality stream at all, which
    /// is what makes [`Self::IMMORTAL`] bit-identical rather than
    /// merely equal in aggregate.
    const fn kills(self) -> bool {
        self.mean_ms > 0
    }

    /// One lifetime in rounds, geometric with mean `mean_ms`, at least
    /// one round. `state` is the mortality stream; the exponential
    /// inverse-CDF is discretised upward, so the realised mean is half
    /// a round above the stated one.
    fn draw_lifetime(self, state: &mut u64) -> u32 {
        let mean_rounds = (self.mean_ms / ROUND_MS) as f64;
        // (0, 1]: the 53 significant bits of the draw, never zero, so
        // the logarithm is always finite.
        let u = ((next_rand(state) >> 11) + 1) as f64 / (1u64 << 53) as f64;
        let life = (-u.ln() * mean_rounds).ceil();
        (life as u32).max(1)
    }
}

/// How much churn is in the room (#412) — a parameter, never a
/// fixture. `Churn::NONE` must reproduce the pre-#412 numbers exactly.
///
/// The two halves are separable on purpose, because they pull in
/// OPPOSITE directions and a single knob would report their sum as if
/// it were one effect:
///
/// - `dials = false` is a peer that only advertises and accepts. It
///   can take a board's fallback dial but never occupies an incoming
///   slot, and against the strict rule — which can never elect a
///   resolvable private address — it is provably inert: the whole
///   `FallbackSpec::Off` row is bit-identical to `Churn::NONE`.
/// - `dials = true` adds what the capture proves the phone also does
///   (a duplicate needs a link in the other role, and `BLE_LINK_REPLACED`
///   appears nine times on `feld-t114`): it dials boards, and every
///   incoming slot it holds makes that board go dark one board-link
///   earlier. That SPREADS the dialling load, which is the very
///   quantity #375 item 3 optimises — so a churning peer measurably
///   helps the strict rule's connectivity while it is robbing the
///   fallback's. The control test pins both directions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Churn {
    /// Churning peers in the room.
    peers: usize,
    /// Whether they dial boards as well as accept dials.
    dials: bool,
}

impl Churn {
    /// An empty room: the pre-#412 simulation.
    const NONE: Self = Self {
        peers: 0,
        dials: false,
    };

    /// `peers` Android Columbas as the room capture shows them.
    const fn phones(peers: usize) -> Self {
        Self { peers, dials: true }
    }

    /// `peers` churning peers that only advertise and accept — the
    /// dial-theft half on its own.
    const fn advertisers(peers: usize) -> Self {
        Self {
            peers,
            dials: false,
        }
    }
}

/// The fourth kind in the rig's room (#412 steady state 2) — a
/// parameter of the same kind as [`Churn`] and [`Mortality`], and
/// `Statics::NONE` reproduces every row measured before it bit for bit.
///
/// # Which of our own things it is
///
/// Identity `e19b2b38912698a9cba4a114fb692857`, the peer that took 42 of
/// `feld-t114`'s 65 outgoing links across the `ble-drop` captures, is
/// **the solar node** — one of our own boards, not a host adapter. The
/// null hypothesis was checked before the kind was modelled, three ways:
///
/// - periculum's own scenario names it:
///   `hardware/ble_mesh_formation_solar.toml` declares
///   `solarnode:e19b2b38`, and `rig-run/solar-20260922T0545Z.log` logs
///   `PARTICIPANTS ... e19b2b38 (solarnode, 1 link-up line)`.
/// - it is not lnsd on a host adapter: every identity file on the rig
///   host (27 of them, `~/.reticulum` and each `*/storage/
///   transport_identity`) was hashed the way Reticulum hashes one and
///   none of them is `e19b2b38`.
/// - and it could not be lnsd anyway, which is the load-bearing half:
///   lnsd advertises `LOCAL_CAPS` unmodified (`bluez.rs:75` —
///   `manufacturer_data_with_hint(LOCAL_CAPS, &hint)`, no
///   `with_free_slots`), so it states no free-slot count, while this
///   peer states one in every window (below). Only the firmware's
///   advertising path does that (`columba.rs:285`).
///
/// # What the capture shows it doing, and where 306 was wrong
///
/// One address in all 2416 lines that carry its identity hint
/// (`d916e2923ed2`, top bits `11` — a static random address, so the
/// shipped [`FallbackOrder::RotatingLast`] never demotes it), and it
/// **does advertise a capability record**: `caps_record=1` with a valid
/// free-slot count in all 2450 `BLE_SCAN_DECISION` lines the three
/// boards logged for it — `free_slots=3` in 1835 of them and
/// `free_slots=2` in 581. 306's model of it ("advertises no slot count,
/// so it deficits by zero like a phone") is therefore wrong in its
/// mechanism and right in its effect: it deficits by zero because it is
/// genuinely EMPTY in three windows out of four, not because it is
/// silent.
///
/// It dials, like [`Churn`]'s `dials = true` half: `feld-pocket` logged
/// 76 identity writes from it (`[BLE ] peer id: e19b2b38`) and never
/// dialled it once, `feld-t114` 6 and `t114-boot` 2 — 84 links it
/// initiated. That follows from its address rather than from a
/// parameter: `d916e2923ed2` is BELOW all three boards
/// (`dcb18ad99cdb` < `e1ac9eb3f05c` < `e81d77f09cdc`), so
/// [`should_initiate`] hands it every board strictly (`initiate_lower_
/// address`) and hands no board a strict verdict for it — every one of
/// the boards' 42 + 1 dials to it is `rule=initiate_fallback`, which is
/// exactly what the `BLE_SCAN_WINDOW` lines say.
///
/// So the kind is: a static, non-rotating address; a real record with a
/// real count; one outgoing slot and [`PERIPH_SLOTS`] incoming ones,
/// like the board it is; sessions that end on the same [`Mortality`]
/// draw a board-to-board session ends on (the distribution was measured
/// over `BLE_CENTRAL_UP`/`DOWN` pairs that INCLUDE this peer's, so the
/// two are one population). What it is not is a member of the board
/// graph the `disc`, `bb` and `boardless` columns count — those stay the
/// `n` boards under study, so a link to the static peer counts in
/// neither, exactly as a link to a phone does not. On the rig it IS a
/// mesh node, so those columns OVERSTATE what a static peer costs the
/// mesh; the column that answers #412 is `stat%`, where the dial went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Statics {
    /// Static peers in the room.
    peers: usize,
    /// Whether they dial boards as well as accept dials.
    dials: bool,
    /// Whether their address is drawn BELOW every board's, which is the
    /// rig's own configuration (`d916e2923ed2` under all three boards).
    /// `false` draws it uniformly in the static-random class, like a
    /// board's, so the peer is the lowest in the room about one order in
    /// `n + 1`.
    lowest: bool,
}

impl Statics {
    /// The rooms measured before this section: boards and phones only.
    const NONE: Self = Self {
        peers: 0,
        dials: false,
        lowest: false,
    };

    /// The rig's own fourth kind: static address below every board's,
    /// dialling boards as the capture's 84 initiated links show.
    const fn rig(peers: usize) -> Self {
        Self {
            peers,
            dials: true,
            lowest: true,
        }
    }

    /// The same peer with its address drawn like a board's — the rig's
    /// address order was one draw of four and this is the rest of them.
    const fn drawn(peers: usize) -> Self {
        Self {
            peers,
            dials: true,
            lowest: false,
        }
    }

    /// The accept-only half on its own: it advertises and takes dials
    /// but initiates none, so a row measures the slot theft without the
    /// load-spreading its own links do.
    const fn accepting(peers: usize) -> Self {
        Self {
            peers,
            dials: false,
            lowest: true,
        }
    }
}

/// The dial ledger (#412 part 2) — a parameter of the same kind as
/// [`Churn`], [`Mortality`] and [`Statics`], and `Ledger::NONE`
/// reproduces every row 311 measured bit for bit.
///
/// # What it is for
///
/// Every candidate KEY measured in this file orders the peers a window
/// heard. `solo%` is the share of the top board's churn dials in which
/// the window heard exactly one thing it was allowed to dial, and it is
/// 100 % on every immortal row, 57 % of the rig room's churn dials when
/// links stand and 38 % at the 600 s mean (306 §3, 311 §4). No order can
/// defend that window: there is nothing to prefer. Only a memory of what
/// the last dials BOUGHT can, and that is this.
///
/// # Why the key is the identity and the effect is board-wide
///
/// The two are one fact, not two decisions. The identity behind an
/// advertisement is unknowable before connecting
/// (`columba.rs:1723`) — the board learns it from the Identity
/// characteristic, post-connect — so a table keyed by identity cannot be
/// consulted per candidate at all. What it CAN do is decide whether this
/// board may spend a fallback dial at all right now, which is what the
/// order calls "strict verdicts only, no fallback, longer pause": while
/// the hold stands the board dials strict verdicts (a board below it in
/// the address sort) and nothing else, and a window holding only peers it
/// would have to reach for with a fallback verdict is a window it leaves
/// alone.
///
/// Keying by identity rather than by address is what lets the count
/// reach `k` at all: the firmware's address table
/// (`columba.rs:1660`, fed at `:867` and `:2050`) forgets a rotating peer
/// at every rotation, which is the hole #412 is about — five addresses in
/// four minutes, so each entry is a first offence forever.
///
/// # What counts as wasted
///
/// One dial, one outcome, against the peer's identity:
///
/// - refused post-connect because that identity was already live (the
///   rotated-address duplicate, `columba.rs:867`): wasted, always. The
///   connect, the discovery and the identity read were paid for and the
///   session never existed.
/// - a session that ended below `t_ms`: wasted. The dial was paid, the
///   link carried nothing.
/// - a session that ended at or above `t_ms`: not wasted, and the
///   identity's count goes back to zero. The ledger remembers a RUN of
///   waste, never a total.
///
/// A link still standing when the run ends has no outcome yet and is not
/// recorded, which is the same rule the `short` column already follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ledger {
    /// Wasted dials to ONE identity, in a row, before the board stops
    /// spending fallback dials. Zero keeps no ledger at all.
    k: u32,
    /// The session length below which a dial counts as wasted.
    t_ms: u64,
    /// How long the board then goes without a fallback dial, in rounds,
    /// counted from the wasted outcome that armed it. Every further
    /// wasted dial to a condemned identity re-arms it, so a condemned
    /// peer is reached for at most once per pause.
    pause_rounds: u32,
}

impl Ledger {
    /// No ledger: every row measured before #412 part 2.
    const NONE: Self = Self {
        k: 0,
        t_ms: 0,
        pause_rounds: 0,
    };

    /// The pause the tables are measured with: the firmware's own
    /// [`DEAD_END_TTL_MS`], 120 s, which is what it already waits after
    /// a dial that bought nothing — the same period, keyed by identity
    /// instead of by address.
    ///
    /// One *scan cycle* ([`FALLBACK_AFTER_ROUNDS`], 30 s), which the
    /// order proposed, cannot be the default and the reason is
    /// structural rather than measured: a teardown resets the strict
    /// clock (`conn_link_down` -> `note_strict_reset`), so the board
    /// already spends one full scan cycle in strict mode before it can
    /// reach a fallback dial. A pause of exactly that length expires in
    /// the round the fallback becomes available and suppresses nothing.
    /// `control_the_dial_ledger_is_a_parameter_and_every_mechanism_fires`
    /// measures that inertness rather than asserting it.
    const PAUSE_ROUNDS: u32 = rounds(DEAD_END_TTL_MS);

    /// The defaults the tables are measured with: `k` wasted dials in a
    /// row, `t_ms` = [`USEFUL_SESSION_MS`] (the most generous bound
    /// available, so the ledger under-states the waste rather than
    /// manufacturing it) and [`Self::PAUSE_ROUNDS`].
    const fn after(k: u32) -> Self {
        Self {
            k,
            t_ms: USEFUL_SESSION_MS,
            pause_rounds: Self::PAUSE_ROUNDS,
        }
    }

    /// The same with one of the two other parameters moved, for the
    /// sweeps: the threshold a session has to reach, and the pause.
    const fn with_t(k: u32, t_ms: u64) -> Self {
        Self {
            k,
            t_ms,
            pause_rounds: Self::PAUSE_ROUNDS,
        }
    }

    const fn with_pause(k: u32, pause_rounds: u32) -> Self {
        Self {
            k,
            t_ms: USEFUL_SESSION_MS,
            pause_rounds,
        }
    }

    /// Whether this configuration keeps a ledger at all. When false
    /// nothing is recorded and nothing is consulted, which is what makes
    /// [`Self::NONE`] bit-identical rather than merely equal in
    /// aggregate.
    const fn keeps(self) -> bool {
        self.k > 0
    }

    /// The row as the SHARED rule states it
    /// ([`leviculum_ble_tx::LedgerPolicy`]), which is the rule every
    /// number in the tables below is measured through. The harness holds
    /// no second copy of the rule: it states the policy in the units the
    /// rest of the file is written in (rounds) and hands it over.
    const fn policy(self) -> LedgerPolicy {
        LedgerPolicy {
            wasted_run: self.k,
            useful_session_ms: self.t_ms,
            pause_ms: self.pause_rounds as u64 * ROUND_MS,
        }
    }
}

/// A dial that never becomes a link (#412, the instrument's first
/// limit) — a parameter of the same kind as [`Churn`], [`Mortality`],
/// [`Statics`] and [`Ledger`], and `ConnectFailure::NONE` reproduces
/// every row measured before it bit for bit.
///
/// # Why it is two different shapes and not one probability
///
/// Measured on the same three `ble-drop` captures the rest of this file
/// reads (`/home/lew/rig-run/ble-drop/{feld-t114,t114-boot,feld-pocket}.
/// log`, 2026-09-12 to 2026-09-27), by pairing each
/// `BLE_CENTRAL_CONNECT addr=A` with the `BLE_CENTRAL_FAIL addr=A` or
/// `BLE_CENTRAL_UP` that follows it on the same board. 3034 dials.
///
/// The failure rate is NOT one number per board, and it is not one
/// number per room either. It splits by the peer's address class, and
/// the two halves have different SHAPES:
///
/// - **A peer with a static random address** (a board, the solar node)
///   fails memorylessly, about a third of the time: 91 of 260 such dials
///   failed across the three boards (35.0 %), and each of the three
///   boards is inside 31 % to 42 % of that on its own. The same address
///   both succeeds and fails — `P(fail | the last dial to this address
///   failed)` is 0.48 to 0.69 against a base of 0.31 to 0.40, so there
///   is some stickiness, but nothing like a fixed per-peer verdict.
/// - **A peer with a rotating address** does not fail per dial at all:
///   it is reachable or it is not, and the ADDRESS decides. Of the 471
///   resolvable-private addresses the three boards dialled, 38 reached
///   the Reticulum service and every single dial to those 38 came up;
///   433 never reached it and every single dial to those failed. Not one
///   address is mixed. So the draw belongs to the rotation, not to the
///   dial, and that is how [`Churner::reachable`] is drawn.
///
/// # What the two levels are
///
/// The rotating population is two kinds wearing one address class, and
/// the harness has a model for only one of them:
///
/// - the Android Columba this file's [`Churner`] is: 38 of 38 of the
///   addresses that reached a Reticulum service came up on the first
///   dial, so its measured failure rate is ZERO ([`Self::CAPTURE`]).
/// - and a rotating advertiser that passes the scanner's filter and has
///   no Reticulum service at all: `stage=discover err=ServiceNotFound`,
///   430 of `feld-t114`'s 450 addresses, and 2655 of its 2675 dials.
///   [`Self::CAPTURE_STRANGERS`] is the room it was in.
///
/// The second is not a worse seed of the first. It is what took
/// `feld-t114` from the 758/707 the module docs quote to 2729/2678: from
/// 2026-09-22 the board's rotating-address dials stopped succeeding
/// altogether, 2652 of 2652, while its dials to the solar node kept
/// failing at the same 42 % they always had.
///
/// # What the firmware does with a failure, which is not one thing
///
/// The stage decides, and only one of the five stages condemns anything
/// (`columba.rs:2124`-`:2318`):
///
/// - `stage=connect` — `central::connect` timed out, no connection ever
///   existed. A FALLBACK dial writes `note_dead_end(addr,
///   "fallback_connect")`, the 120 s address skip; a strict dial writes
///   nothing (#375 §0). `conn_link_up` never fired either, so the strict
///   clock is NOT reset and a stranded board stays in fallback for its
///   next pass, which is exactly what `columba.rs:2598` is for.
/// - `stage=discover`, `identity`, `subscribe`, `handshake` — the
///   connection came up and went down again. No dead end is written at
///   ANY of them, and `conn_link_up` has already reset the strict clock.
///
/// That split is load-bearing rather than a detail: 92 % of
/// `feld-t114`'s failures are post-connect, so 92 % of them leave the
/// address in the pool and the board dials it again next pass. It is why
/// a single unreachable rotating address costs six dials (median 6,
/// max 13, over the 430) instead of one.
///
/// Measured per kind, as the share of failures that happen at
/// `stage=connect`: 34 % for a static-addressed peer (39 of 114), 7.5 %
/// for a rotating one (201 of 2661).
///
/// # What a failed dial costs, measured
///
/// Not the failure itself — `BLE_CENTRAL_CONNECT` to `BLE_CENTRAL_FAIL`
/// is a median 4.93 s at `stage=connect` (the `CONNECT_TIMEOUT_10MS`
/// bound) and 2.28 s at `stage=discover` — but the gap to the board's
/// NEXT `BLE_CENTRAL_CONNECT`, which is the round the room loses. Median
/// 16.35 s after a connect-stage failure and 11.56 s after a
/// post-connect one, against [`ROUND_MS`] = 5 s: three rounds and two.
/// Those are [`FAIL_CONNECT_ROUNDS`] and [`FAIL_POST_ROUNDS`], and the
/// board neither scans nor dials while they run — its strict clock does,
/// because the firmware's is a wall clock.
///
/// # What it does NOT model, stated rather than ranked
///
/// - The churning peer's own dials and the static peer's own dials never
///   fail. Both are measured off the BOARDS' logs, which see a peer's
///   successful dial (it arrives as a link) and cannot see its failed
///   one at all.
/// - The stickiness of the static-peer rate is dropped: the draw is
///   memoryless, which under-states the runs.
/// - A rotating stranger is modelled as the [`Churner`] being
///   unreachable, so at [`Self::CAPTURE_STRANGERS`] the room has no
///   dialable phone at all rather than a phone AND a stranger. That is
///   the room `feld-t114` was in from 2026-09-22 on; a room with both is
///   a fifth kind and not this parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ConnectFailure {
    /// Per mille: a dial to a peer with a STATIC random address — a
    /// board or the solar node — that does not end in a link. Drawn per
    /// dial, memoryless.
    static_fail: u32,
    /// Per mille of those failures that happen at `stage=connect`, the
    /// only stage that condemns an address.
    static_at_connect: u32,
    /// Per mille: a ROTATING peer's CURRENT address is one no dial can
    /// get a Reticulum service out of. Drawn once per address — at the
    /// rotation, not at the dial — because that is the shape the capture
    /// has.
    rotating_dead: u32,
    /// Per mille of a rotating peer's failures that happen at
    /// `stage=connect`.
    rotating_at_connect: u32,
}

impl ConnectFailure {
    /// Every dial connects: the instrument as every row before this
    /// section measured it. No draw is taken from the failure stream at
    /// all, which is what makes it bit-identical rather than merely
    /// equal in aggregate.
    const NONE: Self = Self {
        static_fail: 0,
        static_at_connect: 0,
        rotating_dead: 0,
        rotating_at_connect: 0,
    };

    /// The captures' own rates for the two kinds this harness models: a
    /// static-addressed peer fails 35.0 % of the time (91 of 260) and a
    /// third of those failures (34.2 %, 39 of 114) never connect; the
    /// Android Columba the [`Churner`] is fails not at all (0 of 38
    /// addresses that reached its service).
    const CAPTURE: Self = Self {
        static_fail: 350,
        static_at_connect: 342,
        rotating_dead: 0,
        rotating_at_connect: 76,
    };

    /// The same, plus the rotating population `feld-t114` actually had
    /// from 2026-09-22: 430 of its 450 rotating addresses (95.6 %) never
    /// reached a Reticulum service, and 7.5 % of the failures were at
    /// `stage=connect`. The phone is in the room, wins the fallback and
    /// cannot be linked.
    const CAPTURE_STRANGERS: Self = Self {
        static_fail: 350,
        static_at_connect: 342,
        rotating_dead: 956,
        rotating_at_connect: 76,
    };

    /// Whether any dial can fail in this configuration. When false the
    /// failure stream is never read, so [`Self::NONE`] cannot move a
    /// single draw of the five streams above it.
    const fn fires(self) -> bool {
        self.static_fail > 0 || self.rotating_dead > 0
    }

    /// One draw against a per-mille rate.
    fn hits(per_mille: u32, state: &mut u64) -> bool {
        next_rand(state) % 1000 < u64::from(per_mille)
    }
}

/// Rounds a dial that failed at `stage=connect` costs its board before
/// it scans again: the measured median gap from that dial's
/// `BLE_CENTRAL_CONNECT` to the board's next one is 16.35 s, and a round
/// is [`ROUND_MS`].
const FAIL_CONNECT_ROUNDS: u32 = 3;

/// The same for a failure past `central::connect` — the connection came
/// up and the discovery, identity read, subscribe or handshake did not.
/// Measured median gap 11.56 s.
const FAIL_POST_ROUNDS: u32 = 2;

/// The ATT MTU handed to the duplicate rule for BOTH links of a
/// duplicate pair.
///
/// The value cannot matter: [`judge_duplicate`] only compares the two
/// against each other, and a phone's stack negotiates the same MTU in
/// either role, so they are equal and the rule falls through to its
/// identity tie-break. Using a real constant rather than a number
/// keeps an invented figure out of the model.
const SIM_USABLE_MTU: u16 = MIN_USABLE_MTU;

/// When the fallback clock may run (#375 part 2, item 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FallbackSpec {
    /// No fallback at all: the strict rule as published (the control).
    Off,
    /// The shipped spec (part 3): the clock runs whenever the outgoing
    /// slot is free, live incoming links notwithstanding.
    Eager,
    /// Part 2's spec, kept as the record: the clock is suspended (held
    /// at zero) while the board has ANY live link in either role; only
    /// a fully linkless board may dial against the sort.
    Quiet,
}

/// Which eligible advertiser a scanning board dials (#375 part 2, item 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetChoice {
    /// Whichever eligible PDU the radio heard first — arbitrary, so
    /// seeded random here (as the firmware behaved before the window).
    FirstSeen,
    /// One scan window collected, then the lowest-addressed eligible
    /// candidate, strict verdicts before fallback verdicts — the real
    /// [`CandidateTable`] policy as item 2 shipped it, with no peer
    /// advertising a slot count.
    LowestEligible,
    /// What #375 item 3 shipped and #412 measured the cost of: the
    /// same window and the same table, with every board advertising how
    /// many incoming slots it still has, so the fullest peers sort
    /// behind the emptiest ones and only equal counts fall back to the
    /// address.
    MostFreeSlots,
    /// [`Self::MostFreeSlots`] with the fallback class's last term
    /// changed from the address to which advertiser the window heard
    /// first ([`FallbackOrder::FirstHeard`]). Everything else is
    /// identical, including the table, so the difference between this
    /// row and `MostFreeSlots` is that one term and nothing else.
    /// Measured, not shipped: see the module docs for what it costs
    /// the empty room.
    FallbackFirstHeard,
    /// The shipped policy since #412: the same again with
    /// [`FallbackOrder::RotatingLast`], which keeps the address term
    /// for candidates whose address stays put and puts the ones that
    /// redraw it behind them.
    RotatingLast,
    /// Candidate (a) of 306, measured and not built: the shipped key
    /// with its two MIDDLE terms swapped, so the rotating group is
    /// asked ABOVE the free-slot deficit — `(tier, group, deficit,
    /// address-or-sighting)` instead of `(tier, deficit, group, ...)`.
    ///
    /// It is a harness-side re-offer of the shipped table rather than a
    /// second key: the non-rotating candidates get a window of their
    /// own and the rotating ones are offered only if that window came
    /// out empty (see [`elect`]). That is exactly the swapped key here,
    /// not an approximation of it, because the group term is only ever
    /// set for candidates in the WORST tier ([`DialPreference::
    /// Fallback`], `window.rs`'s `rank`), so no rotating candidate can
    /// outrank a non-rotating one on the tier the swap moved it above.
    GroupAboveDeficit,
    /// The other way to take the phone's free ride away without
    /// touching the group order: a peer that advertised NO capability
    /// record deficits as if it were FULL instead of empty. Silence is
    /// not "all slots free".
    ///
    /// Also harness-side, and also the real table: a recordless
    /// candidate is offered as `Some(0)` free slots instead of `None`,
    /// which is what the window would read off an advertisement that
    /// said so. Everything else — tier, group, address — is the shipped
    /// key. In a room where every peer advertises a count (an empty room
    /// of boards, or one with a static peer and no phone in it) this row
    /// is the shipped row bit for bit, which the control asserts.
    SilenceIsFull,
}

impl TargetChoice {
    /// Whether the boards in this configuration advertise their free
    /// incoming slots (#375 item 3).
    fn advertises_slots(self) -> bool {
        matches!(
            self,
            Self::MostFreeSlots
                | Self::FallbackFirstHeard
                | Self::RotatingLast
                | Self::GroupAboveDeficit
                | Self::SilenceIsFull
        )
    }

    /// The tie-break the shared table uses for the fallback class.
    fn fallback_order(self) -> FallbackOrder {
        match self {
            Self::FallbackFirstHeard => FallbackOrder::FirstHeard,
            Self::RotatingLast | Self::GroupAboveDeficit | Self::SilenceIsFull => {
                FallbackOrder::RotatingLast
            }
            _ => FallbackOrder::Address,
        }
    }

    /// Whether the row reads the order the advertising PDUs arrived
    /// in. Only the two orders that break a tie by it draw the shuffle
    /// — a row that never reads it must not consume the draw either,
    /// or the two would not be the same replay.
    fn reads_arrival_order(self) -> bool {
        matches!(
            self,
            Self::FallbackFirstHeard
                | Self::RotatingLast
                | Self::GroupAboveDeficit
                | Self::SilenceIsFull
        )
    }
}

/// Whether the shipped table puts this candidate in the fallback class's
/// ROTATING group — read off the table itself rather than restated here.
///
/// The rule lives in `window.rs` (`rotating_address`, the top two
/// address bits) and is not exported, and a second copy of it in the
/// harness would be a second policy. So the question is asked of the
/// real table: offer the candidate against a probe that ties with it on
/// every other term — same verdict, same slot count — at the LAST
/// address of the non-rotating class. The probe wins iff the candidate
/// sits in the group behind it.
fn in_rotating_group(addr: u64, decision: ConnectDecision, free: Option<u8>) -> bool {
    /// `11` on top, so never a rotating address, and the highest one
    /// there is: any non-rotating candidate beats it on the address
    /// term, and no drawn address can equal it.
    const PROBE: u64 = 0xFFFF_FFFF_FFFF;
    assert_ne!(addr, PROBE, "the probe's address is in the room");
    let mut table: CandidateTable<bool, 2> =
        CandidateTable::with_fallback_order(FallbackOrder::RotatingLast);
    table.offer(addr, decision, free, false);
    table.offer(PROBE, decision, free, true);
    table
        .into_best()
        .map(|(_, _, probe_won)| probe_won)
        .expect("an initiate verdict is a candidate")
}

/// One collected window's election under `choice`, through the real
/// [`CandidateTable`] in every case — including the two policies #412
/// steady state 2 measures, which are re-offers and not a second key
/// (see [`TargetChoice::GroupAboveDeficit`] and
/// [`TargetChoice::SilenceIsFull`]).
///
/// `offers` is `(peer index, address, verdict, advertised free slots)`
/// in the order the advertising PDUs arrived. The tier census happens
/// here, once per offer, whatever the policy does with the offer
/// afterwards.
fn elect(
    choice: TargetChoice,
    offers: &[(usize, u64, ConnectDecision, Option<u8>)],
    tally: &mut Tally,
) -> usize {
    let silence_is_full = choice == TargetChoice::SilenceIsFull;
    let read_slots = |free: Option<u8>| {
        if silence_is_full {
            free.or(Some(0))
        } else {
            free
        }
    };
    // 303's tier, censused rather than assumed: the public rule is
    // asked the same question the window's key asks it, per offer.
    for &(_, _, decision, free) in offers {
        tally.offers += 1;
        if dial_preference(decision, read_slots(free)) == DialPreference::CannotDialUs {
            tally.offers_cannot_dial += 1;
        }
    }
    let pass = |group: Option<bool>| -> Option<usize> {
        let mut window: CandidateTable<usize, WINDOW_CANDIDATES> =
            CandidateTable::with_fallback_order(choice.fallback_order());
        for &(p, addr, decision, free) in offers {
            let free = read_slots(free);
            if group.is_some_and(|want| in_rotating_group(addr, decision, free) != want) {
                continue;
            }
            window.offer(addr, decision, free, p);
        }
        window.into_best().map(|(_, _, p)| p)
    };
    if choice == TargetChoice::GroupAboveDeficit {
        // The group term first: the candidates whose address is a key
        // get the window, and the ones that redraw it are only asked
        // when that window is empty.
        pass(Some(false))
            .or_else(|| pass(Some(true)))
            .expect("a non-empty candidate set chooses")
    } else {
        pass(None).expect("a non-empty candidate set chooses")
    }
}

/// xorshift64* — deterministic, seedable, no dependency.
fn next_rand(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *state = x;
    x.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

/// A 16-byte identity, derived from the address a peer was created
/// with. v2.2 keys everything durable by identity, and the only two
/// properties the harness needs are the ones #412 is about: a churning
/// peer keeps ONE identity across every rotation, and no two peers
/// share one.
fn identity_from(addr: u64) -> [u8; 16] {
    let mut identity = [0u8; 16];
    identity[..8].copy_from_slice(&addr.to_le_bytes());
    identity[8..].copy_from_slice(&addr.rotate_left(32).to_be_bytes());
    identity
}

/// One live link as a board holds it.
///
/// `addr` is the address the connection was made on, which is what the
/// Core Spec §4.5 exclusion compares — for a churning peer that is an
/// address it may already have abandoned, and the gap between the two
/// is the hole #412 is about.
#[derive(Debug, Clone, Copy)]
struct Link {
    /// Index in the peer space: `< n` a board, `>= n` a churning peer.
    peer: usize,
    addr: u64,
    formed: u32,
    /// When the peer stopped answering on this link (its rotation), if
    /// it has. The link still holds its slot until
    /// [`LINK_EXPIRY_ROUNDS`] later, exactly as the registry's expiry
    /// sweep holds it.
    silent_since: Option<u32>,
    /// The round this board-to-board link ends in, drawn once at
    /// formation from [`Mortality`]. `None` under
    /// [`Mortality::IMMORTAL`] and on every link to a churning peer —
    /// those end at the peer's next rotation, which is a measured
    /// period and not a drawn one.
    dies_at: Option<u32>,
}

impl Link {
    /// How long the link actually carried, in milliseconds: up to the
    /// moment the peer went silent, or to `now` while it still answers.
    fn useful_ms(&self, now: u32) -> u64 {
        u64::from(self.silent_since.unwrap_or(now).saturating_sub(self.formed)) * ROUND_MS
    }
}

struct Board {
    /// 48-bit static-random address, distinct per board.
    addr: u64,
    identity: [u8; 16],
    arrived: bool,
    /// The one central link.
    outgoing: Option<Link>,
    /// Peripheral links, at most [`PERIPH_SLOTS`].
    incoming: Vec<Link>,
    /// Whether this board has already held an outgoing link in this
    /// run — set when one FORMS, so a dial refused post-connect does not
    /// set it: the slot was never occupied and the next dial is the same
    /// free slot, not a freed one. Every dial made once this is set spends
    /// a slot that was freed (#412 steady state), which is the quantity
    /// the rig read directly: `feld-t114`'s outgoing slot was free seven
    /// times over two days.
    held_outgoing: bool,
    /// Empty scan rounds since the last link-up or permitted peer —
    /// the sim's copy of the firmware's fallback clock.
    strict_rounds: u32,
    /// The firmware's `DEAD_ENDS` table, per board: address and the
    /// round its entry expires.
    dead_ends: Vec<(u64, u32)>,
    /// The #412 part 2 ledger, as the shared type holds it
    /// ([`leviculum_ble_tx::DialLedger`]): per identity the run of dials
    /// this board paid for and got nothing back from, and the one pause
    /// those runs arm.
    ledger: DialLedger,
    /// The round this board may scan again, after a dial that did not
    /// connect ([`ConnectFailure`]). Zero is a board that is not in a
    /// backoff, which every row measured before that parameter is.
    busy_until: u32,
}

impl Board {
    /// The §4.5 exclusion: a connection on this address already exists,
    /// so a dial to it can only time out.
    fn addr_linked(&self, addr: u64) -> bool {
        self.outgoing
            .iter()
            .chain(&self.incoming)
            .any(|l| l.addr == addr)
    }

    fn dead_end(&self, addr: u64, round: u32) -> bool {
        self.dead_ends
            .iter()
            .any(|&(a, until)| a == addr && round < until)
    }

    /// Condemn an address, evicting the oldest entry when full.
    fn note_dead_end(&mut self, addr: u64, round: u32) {
        let until = round + rounds(DEAD_END_TTL_MS);
        if let Some(entry) = self.dead_ends.iter_mut().find(|(a, _)| *a == addr) {
            entry.1 = until;
            return;
        }
        if self.dead_ends.len() < DEAD_END_SLOTS {
            self.dead_ends.push((addr, until));
            return;
        }
        let oldest = self
            .dead_ends
            .iter_mut()
            .min_by_key(|(_, until)| *until)
            .expect("the table is full, so it is not empty");
        *oldest = (addr, until);
    }

    /// One dial outcome, against the identity the dial turned out to
    /// have (#412 part 2), through the shared rule itself — `round` is
    /// the harness's clock and [`ROUND_MS`] is what one round is worth,
    /// which is the only conversion between the two.
    ///
    /// `session_ms` is `None` for a dial refused post-connect: there was
    /// no session.
    fn note_dial_outcome(
        &mut self,
        ledger: Ledger,
        identity: &[u8; 16],
        session_ms: Option<u64>,
        round: u32,
    ) {
        self.ledger.note(
            ledger.policy(),
            identity,
            session_ms,
            u64::from(round) * ROUND_MS,
        );
    }

    /// The live link, if any, that already belongs to this identity —
    /// what the firmware finds post-connect when it reads the Identity
    /// characteristic, and the only place a rotated address is ever
    /// recognised.
    fn link_with(&self, peer: usize) -> Option<(Origin, Link)> {
        if let Some(link) = self.outgoing.filter(|l| l.peer == peer) {
            return Some((Origin::Outgoing, link));
        }
        self.incoming
            .iter()
            .find(|l| l.peer == peer)
            .map(|&link| (Origin::Incoming, link))
    }
}

/// The #412 churning peer: one identity, a new address every
/// [`CHURN_ROTATE_MS`], always advertising, central-capable, accepting
/// links whose sessions end at its next rotation.
///
/// It carries no v0.3.0 capability record (`caps: None`), because an
/// Android Columba does not advertise one — which per v0.3.0 §3.2 reads
/// as full capability, and per [`crate::window`]'s ranking as "all
/// slots free". Its address is drawn from the resolvable-private class
/// (top two bits `01`, Core Spec Vol 6 Part B §1.3.2.2), so it is
/// structurally BELOW every static-random board address: the sort never
/// lets a board dial it, and the fallback class always ranks it first.
/// `peer.rs`'s `any_rpa_sorts_below_any_static_random_address` pins
/// that fact; this is what it costs.
struct Churner {
    addr: u64,
    identity: [u8; 16],
    /// Round within the rotation cycle at which it re-draws.
    phase: u32,
    /// Boards it has dialled since its last rotation — its own central
    /// slots, bounded by [`CHURN_CENTRAL_LINKS`].
    links: Vec<usize>,
    /// Whether a dial to the address it currently wears can reach a
    /// Reticulum service at all ([`ConnectFailure`]). Drawn once per
    /// address rather than per dial, because that is the shape the
    /// captures have: of 471 rotating addresses the boards dialled, 38
    /// came up on every dial and 433 failed on every dial, and not one
    /// was mixed. Always true under [`ConnectFailure::NONE`], which
    /// takes no draw.
    reachable: bool,
}

/// The #412 static peer: the solar node as [`Statics`] describes it —
/// one fixed static-random address, a capability record with its real
/// free-slot count, one outgoing slot and [`PERIPH_SLOTS`] incoming
/// ones. It never goes silent (nothing rotates), so no link to it ever
/// reaches the expiry sweep; its sessions end on a [`Mortality`] draw
/// like a board's.
struct StaticPeer {
    addr: u64,
    /// Its 16-byte identity — the solar node's `e19b2b38...` is one
    /// identity under one address, so the ledger's key and the §4.5
    /// address exclusion agree about it, which is the whole difference
    /// from the phone.
    identity: [u8; 16],
    /// Its one central link, and the round it dies in.
    outgoing: Option<Link>,
    /// Peripheral links, at most [`PERIPH_SLOTS`] — the count it
    /// advertises is this vector's complement, which is why it reads as
    /// "all free" in three windows out of four.
    incoming: Vec<Link>,
    /// Its own fallback clock. It only ever runs when the peer is NOT
    /// the lowest address in the room ([`Statics::drawn`]); at the rig's
    /// own address order every board is a strict candidate for it.
    strict_rounds: u32,
}

/// What a run spent and what it got, per #412's three numbers.
///
/// `dials` counts every dial that reached the identity read, which is
/// where a spent one is recognised; `dials == board_links + churn_links
/// + static_links + refused` is checked at the end of every run.
#[derive(Debug, Default, Clone, Copy)]
struct Tally {
    dials: usize,
    /// Dials that formed a board-to-board link. Under
    /// [`Mortality::IMMORTAL`] these never end, so each is useful; with
    /// mortality on, one that died below [`USEFUL_SESSION_MS`] is
    /// counted in `short` like a churn session, and `useful` keeps
    /// meaning what it says.
    board_links: usize,
    /// Dials that formed a link to a churning peer, replacements
    /// included.
    churn_links: usize,
    /// Dials that formed a link to a [`StaticPeer`] — the rig's fourth
    /// kind. No replacement can be one: the peer's address never
    /// changes, so the §4.5 address exclusion catches the duplicate
    /// before the dial, which the rotation is what defeats.
    static_links: usize,
    /// Dials refused post-connect because the identity was already
    /// live — the rotated-address duplicate, spent by definition.
    refused: usize,
    /// Dials that never became a link because the dial itself did not
    /// get that far ([`ConnectFailure`]): the connect timed out, or the
    /// service discovery, identity read, subscribe or handshake failed.
    /// Zero under [`ConnectFailure::NONE`] by construction.
    failed: usize,
    /// Of those: the ones aimed at a churning peer, and the ones that
    /// failed at `stage=connect` and therefore condemned the address for
    /// [`DEAD_END_TTL_MS`] when the verdict was a fallback one.
    failed_churn: usize,
    failed_at_connect: usize,
    /// Links whose session ended below [`USEFUL_SESSION_MS`]: the dial
    /// was paid, the link carried nothing. Without [`Mortality`] only a
    /// link to a churning peer can be one.
    short: usize,
    /// Board-to-board links that reached their drawn lifetime — the
    /// mortality model's own positive control.
    deaths: usize,
    /// Dials made by a board that had already held an outgoing link:
    /// every one of them spends a slot that was freed, so this is the
    /// steady-state denominator the formation phase has no equivalent
    /// of.
    refill_dials: usize,
    /// Of those, the ones aimed at a churning peer.
    refill_dials_churn: usize,
    /// And the ones aimed at a static peer (#412 steady state 2) — the
    /// dials the rig's captures show going to the solar node.
    refill_dials_static: usize,
    /// The same pair for the HIGHEST-ADDRESSED board alone, which is the
    /// board the rig read: `feld-t114` holds `e81d77f09cdc`, above both
    /// `t114-boot` and `feld-pocket`, and its own `BLE_SCAN_DECISION`
    /// lines say `wait_peer_lower_address` for both of them 2301 times
    /// and `initiate_lower_address` for neither, so the v2.2 rule leaves
    /// it no strict board candidate in that room and every dial it makes
    /// is a fallback dial. A room average mixes that board with the ones
    /// that have a strict candidate and cannot be held against its 7 of
    /// 7.
    refill_dials_top: usize,
    refill_dials_top_churn: usize,
    /// And the top board's refill dials that went to a static peer —
    /// the column #412 steady state 2 asks for, because on the rig it is
    /// where 42 of `feld-t114`'s 65 outgoing links went and all 25 of
    /// the ones it made after the shipped order reached the boards.
    refill_dials_top_static: usize,
    /// Of those: how many had NO board among the permitted candidates at
    /// all, so the churning peer was not preferred over a board but was
    /// the only thing the rule allowed. A tie-break cannot defend a slot
    /// in that window, whatever it orders by, and this counts how often
    /// the window is that one.
    refill_dials_top_churn_solo: usize,
    /// Window offers, and how many of them [`dial_preference`] put in
    /// [`DialPreference::CannotDialUs`] — 303's tier, counted rather
    /// than assumed inert. Only the window choices offer anything, so
    /// [`TargetChoice::FirstSeen`] leaves both at zero.
    offers: usize,
    offers_cannot_dial: usize,
    /// Windows the [`Ledger`] held shut (#412 part 2): the board had
    /// reached its fallback bound, the ledger's pause stood, the strict
    /// window was empty and the fallback window would NOT have been. So
    /// this counts the dials the ledger refused to make — nothing else in
    /// the model can tell that from a board that simply had nobody to
    /// dial. Zero under [`Ledger::NONE`] by construction.
    held_windows: usize,
    /// Of those: the windows whose fallback candidates held no BOARD at
    /// all — the solo window, the one no candidate ORDER can defend,
    /// which is what the ledger exists to close. The rest are windows
    /// where a board was on offer and the ledger declined it too; that is
    /// the cost side, and `bb` and `disc` are where it shows up.
    held_windows_solo: usize,
    /// The same pair for the highest-addressed board alone — the board
    /// the rig read, and the only one whose `solo%` is comparable to
    /// `feld-t114`'s.
    held_windows_top: usize,
    held_windows_top_solo: usize,
}

impl Tally {
    /// Dials that produced a link that lasted.
    fn useful(&self) -> usize {
        self.board_links + self.churn_links + self.static_links - self.short
    }
}

struct Sim {
    boards: Vec<Board>,
    tally: Tally,
}

/// Everything board `i`'s scan pass would offer its window in `mode`:
/// arrived, advertising (a full board is not, #372), not itself, not
/// already linked to it (the Core Spec §4.5 exclusion), not backed off by
/// the dead-end table — and then the real rule's verdict, keeping only
/// the peers it says to dial.
///
/// `(peer index, address, verdict, advertised free slots)` in peer-space
/// order. A churning peer is always advertising and never full — that is
/// what "always in the room" means.
///
/// It is a function rather than the inline block it was because #412 part
/// 2 needs the same window asked twice: once in the mode the board is
/// allowed to use, and once in [`ScanMode::Fallback`] to count what the
/// ledger's pause refused. Asking it twice is free of randomness — the
/// window reads state and consumes no draw — so the counterfactual cannot
/// move a single row.
fn window_offers(
    i: usize,
    mode: ScanMode,
    round: u32,
    choice: TargetChoice,
    boards: &[Board],
    churners: &[Churner],
    static_peers: &[StaticPeer],
) -> Vec<(usize, u64, ConnectDecision, Option<u8>)> {
    let churn_base = boards.len();
    let static_base = churn_base + churners.len();
    let peers = static_base + static_peers.len();
    (0..peers)
        .filter_map(|p| {
            if p == i {
                return None;
            }
            let free_of = |held: usize| {
                choice
                    .advertises_slots()
                    .then(|| u8::try_from(PERIPH_SLOTS - held).expect("slots fit a byte"))
            };
            let (addr, caps, free) = if p < churn_base {
                if !boards[p].arrived || boards[p].incoming.len() >= PERIPH_SLOTS {
                    return None;
                }
                (boards[p].addr, Some(0), free_of(boards[p].incoming.len()))
            } else if p < static_base {
                // No v0.3.0 record at all: full capability per
                // §3.2, no slot count per item 3.
                (churners[p - churn_base].addr, None, None)
            } else {
                // The rig's fourth kind: a full record with a
                // real count, like the board it is (#372 stops
                // its advertising when the last slot goes).
                let peer = &static_peers[p - static_base];
                if peer.incoming.len() >= PERIPH_SLOTS {
                    return None;
                }
                (peer.addr, Some(0), free_of(peer.incoming.len()))
            };
            if boards[i].addr_linked(addr) || boards[i].dead_end(addr, round) {
                return None;
            }
            let decision = should_initiate(0, boards[i].addr, caps, addr, mode);
            decision.initiate().then_some((p, addr, decision, free))
        })
        .collect()
}

/// Whether the two boards hold a link in either direction. Board
/// addresses are static, so the pre-dial address exclusion (Core Spec
/// §4.5) keeps a linked board from ever being dialled again, and the
/// sim never holds two links between one pair — under [`Mortality`] a
/// pair whose link died may of course form a new one, which is the
/// steady state the section below measures. Churning peers live
/// at indices `>= n` and can never equal a board index, so the board
/// graph this walks is the board graph alone — a phone is an endpoint,
/// not a relay, and two boards that share a phone are not connected.
fn linked(boards: &[Board], a: usize, b: usize) -> bool {
    boards[a].outgoing.is_some_and(|l| l.peer == b)
        || boards[b].outgoing.is_some_and(|l| l.peer == a)
}

/// Replay one arrival order and return the final boards plus the dial
/// ledger. With `churn == 0` and [`Mortality::IMMORTAL`] this is the
/// pre-#412 simulation exactly: the churn and mortality paths draw from
/// their own seeded streams, so the main stream — addresses, arrival
/// order, scan order, first-seen picks — is byte-identical to what it
/// was.
// Nine parameters, and each one is a room or policy knob a table varies
// on its own: the two sizes, the seed, the two #375 policies and the
// four #412 models. A struct around them would move the same list one
// indirection away without removing a call site or a parameter.
#[allow(clippy::too_many_arguments)]
fn run_sim(
    n: usize,
    seed: u64,
    spec: FallbackSpec,
    choice: TargetChoice,
    churn: Churn,
    mortality: Mortality,
    statics: Statics,
    ledger: Ledger,
    failure: ConnectFailure,
) -> Sim {
    let mut rng = seed | 1;
    let mut boards: Vec<Board> = Vec::with_capacity(n);
    while boards.len() < n {
        let addr = (next_rand(&mut rng) & 0xFFFF_FFFF_FFFF) | 0xC000_0000_0000;
        if boards.iter().any(|b| b.addr == addr) {
            continue;
        }
        boards.push(Board {
            addr,
            identity: identity_from(addr),
            arrived: false,
            outgoing: None,
            incoming: Vec::new(),
            held_outgoing: false,
            strict_rounds: 0,
            dead_ends: Vec::new(),
            ledger: DialLedger::new(),
            busy_until: 0,
        });
    }

    // The board the rig's reading is about: the highest-addressed one,
    // which the v2.2 sort leaves without a single strict board candidate,
    // so every dial it makes is a fallback dial.
    let top_board = (0..n)
        .max_by_key(|&i| boards[i].addr)
        .expect("a room has boards");

    // The arrival order under test: a seeded shuffle, one per round.
    let mut arrival: Vec<usize> = (0..n).collect();
    for i in (1..n).rev() {
        arrival.swap(i, (next_rand(&mut rng) as usize) % (i + 1));
    }

    // The churning peers come from a stream of their own, so that
    // adding them cannot move a single draw of the one above.
    let mut churn_rng = (seed ^ 0xC0FF_EE15_0BAD_F00D) | 1;
    // And the advertising arrival order from a third, for the same
    // reason one step further (#412): only `FallbackFirstHeard` reads
    // it, and it must leave both streams above untouched.
    let mut offer_rng = (seed ^ 0x0FFE_5ED0_1DE5_7ABC) | 1;
    // And the link lifetimes from a FOURTH, for the same reason once
    // more (#412 steady state): a draw is taken only when a
    // board-to-board link forms and only while `mortality.kills()`, so
    // `Mortality::IMMORTAL` leaves all three streams above exactly
    // where they were.
    let mut death_rng = (seed ^ 0x0DEA_D111_FE71_3E55) | 1;
    // And the static peers from a FIFTH, for the same reason once more
    // (#412 steady state 2): a draw is taken only while
    // `statics.peers > 0`, so `Statics::NONE` leaves all four streams
    // above exactly where they were.
    let mut static_rng = (seed ^ 0x57A7_1C00_DDE5_C11B) | 1;
    // And the dial failures from a SIXTH, for the same reason once more
    // (#412, the instrument's first limit): a draw is taken only while
    // `failure.fires()`, so `ConnectFailure::NONE` leaves all five
    // streams above exactly where they were.
    let mut fail_rng = (seed ^ 0xFA11_ED00_C0FF_EE21) | 1;
    // Whether a rotating peer's CURRENT address can be reached at all.
    // One draw per address, never per dial — see [`ConnectFailure`].
    let draw_reachable =
        |state: &mut u64| !failure.fires() || !ConnectFailure::hits(failure.rotating_dead, state);
    let mut churners: Vec<Churner> = (0..churn.peers)
        .map(|_| {
            let addr = (next_rand(&mut churn_rng) & 0x3FFF_FFFF_FFFF) | 0x4000_0000_0000;
            Churner {
                addr,
                identity: identity_from(addr),
                phase: (next_rand(&mut churn_rng) as u32) % rounds(CHURN_ROTATE_MS),
                links: Vec::new(),
                reachable: draw_reachable(&mut fail_rng),
            }
        })
        .collect();

    // Where each kind lives in the peer space: boards below `churn_base`,
    // churning peers below `static_base`, static peers above it. Every
    // comparison in the loop below names one of these rather than `n`.
    // ([`window_offers`] derives the same two bounds from the slice
    // lengths it is handed, so the peer space has one definition.)
    let churn_base = n;
    let static_base = churn_base + churn.peers;

    // The static peer's address: a static-random one like a board's
    // (`11` on top), drawn BELOW every board's in the rig's own
    // configuration, where `d916e2923ed2` sat under all three. The
    // `drawn` variant puts it in the same range as a board, so the rig's
    // address order is one draw among `n + 1`.
    const STATIC_FLOOR: u64 = 0xC000_0000_0000;
    let lowest_board = boards
        .iter()
        .map(|b| b.addr)
        .min()
        .expect("a room has boards");
    let mut static_peers: Vec<StaticPeer> = Vec::with_capacity(statics.peers);
    while static_peers.len() < statics.peers {
        let addr = if statics.lowest {
            STATIC_FLOOR + next_rand(&mut static_rng) % (lowest_board - STATIC_FLOOR).max(1)
        } else {
            (next_rand(&mut static_rng) & 0xFFFF_FFFF_FFFF) | STATIC_FLOOR
        };
        if boards.iter().any(|b| b.addr == addr) || static_peers.iter().any(|s| s.addr == addr) {
            continue;
        }
        static_peers.push(StaticPeer {
            addr,
            identity: identity_from(addr),
            outgoing: None,
            incoming: Vec::new(),
            strict_rounds: 0,
        });
    }

    let mut tally = Tally::default();
    // A room where a dial can fail is never provably quiescent either:
    // a board in a failure backoff forms no link, so the linkless streak
    // below would read a room that is still trying as a settled one. It
    // goes to the horizon like every other room with a parameter in it.
    let horizon =
        if churn.peers == 0 && statics.peers == 0 && !mortality.kills() && !failure.fires() {
            10_000
        } else {
            HORIZON_ROUNDS
        };
    let mut linkless_streak: u32 = 0;
    for round in 0..horizon {
        if (round as usize) < n {
            boards[arrival[round as usize]].arrived = true;
        }

        // A rotation abandons every link the peer holds: it re-appears
        // under a new address and the older link dies of the expiry
        // below, `reason="timeout"`, exactly as the room capture shows.
        for (k, churner) in churners.iter_mut().enumerate() {
            if round % rounds(CHURN_ROTATE_MS) != churner.phase {
                continue;
            }
            churner.addr = (next_rand(&mut churn_rng) & 0x3FFF_FFFF_FFFF) | 0x4000_0000_0000;
            // A new address is a new verdict: the captures give one per
            // address and never a mixed one.
            churner.reachable = draw_reachable(&mut fail_rng);
            churner.links.clear();
            for board in boards.iter_mut() {
                for link in board.outgoing.iter_mut().chain(board.incoming.iter_mut()) {
                    if link.peer == n + k && link.silent_since.is_none() {
                        link.silent_since = Some(round);
                    }
                }
            }
        }

        // The expiry sweep. A teardown restarts the strict phase in the
        // firmware (`conn_link_down` -> `note_strict_reset`).
        for board in boards.iter_mut() {
            let expired = |l: &Link| {
                l.silent_since
                    .is_some_and(|since| round >= since + LINK_EXPIRY_ROUNDS)
            };
            if board.outgoing.is_some_and(|l| expired(&l)) {
                let link = board.outgoing.take().expect("just tested");
                if link.useful_ms(round) < USEFUL_SESSION_MS {
                    tally.short += 1;
                }
                // The dial that bought this link has its outcome now
                // (#412 part 2). Only a peer that goes SILENT reaches the
                // expiry sweep, and only a churning peer ever does, so
                // the identity is the churner's — the one identity behind
                // however many addresses it has spent.
                let identity = churners
                    .get(link.peer.wrapping_sub(churn_base))
                    .expect("only a churning peer ever goes silent")
                    .identity;
                board.note_dial_outcome(ledger, &identity, Some(link.useful_ms(round)), round);
                board.strict_rounds = 0;
            }
            let before = board.incoming.len();
            board.incoming.retain(|l| !expired(l));
            if board.incoming.len() != before {
                board.strict_rounds = 0;
            }
        }

        // Link mortality (#412 steady state). A board-to-board link that
        // reached its drawn lifetime ends, BOTH ends in this round: the
        // central's one outgoing slot and the peripheral's incoming slot
        // free together, because a BLE disconnect is symmetric, and both
        // sides reset the strict clock the way `conn_link_down` does.
        // Neither address is condemned — a session that ended is not a
        // dead end, and the firmware's table is for a refusal or a dial
        // that could not connect.
        // A link to a STATIC peer ends the same way and for the same
        // reason: both ends are our own firmware, and the lifetime
        // distribution was measured over a population that includes this
        // peer's own sessions.
        for i in 0..n {
            let Some(link) = boards[i].outgoing else {
                continue;
            };
            if !link.dies_at.is_some_and(|at| round >= at) {
                continue;
            }
            boards[i].outgoing = None;
            if link.useful_ms(round) < USEFUL_SESSION_MS {
                tally.short += 1;
            }
            boards[i].strict_rounds = 0;
            // This board's dial has its outcome (#412 part 2). A session
            // that ended below the ledger's threshold is a wasted dial
            // whoever it was spent on: the ledger has no notion of a peer
            // kind, only of what a dial bought, and a board whose links
            // keep dying young is exactly as bad a target as a phone.
            let identity = if link.peer < churn_base {
                boards[link.peer].identity
            } else {
                static_peers[link.peer - static_base].identity
            };
            boards[i].note_dial_outcome(ledger, &identity, Some(link.useful_ms(round)), round);
            if link.peer < churn_base {
                boards[link.peer].incoming.retain(|l| l.peer != i);
                boards[link.peer].strict_rounds = 0;
            } else {
                let peer = &mut static_peers[link.peer - static_base];
                peer.incoming.retain(|l| l.peer != i);
                peer.strict_rounds = 0;
            }
            tally.deaths += 1;
        }

        // The static peer's OWN outgoing link dies on the same draw. It
        // is not in the boards' ledger — `deaths` counts the links the
        // room's dials paid for, and this one was the peer's dial — but
        // the board whose incoming slot it held gets the slot and its
        // strict clock back, which is the half the boards can see.
        for (k, peer) in static_peers.iter_mut().enumerate() {
            let Some(link) = peer.outgoing else {
                continue;
            };
            if !link.dies_at.is_some_and(|at| round >= at) {
                continue;
            }
            peer.outgoing = None;
            peer.strict_rounds = 0;
            boards[link.peer]
                .incoming
                .retain(|l| l.peer != static_base + k);
            boards[link.peer].strict_rounds = 0;
        }

        // Scan order within the round is part of the replayed
        // randomness: which searching board wins a contended slot.
        let mut scan_order: Vec<usize> = (0..n).collect();
        for i in (1..n).rev() {
            scan_order.swap(i, (next_rand(&mut rng) as usize) % (i + 1));
        }

        let mut any_link = false;
        for i in scan_order {
            // Linked centrals do not scan; unarrived boards are absent.
            if !boards[i].arrived || boards[i].outgoing.is_some() {
                continue;
            }
            // A board still paying for a dial that did not connect is in
            // the connect/backoff path, not in a scan window
            // ([`ConnectFailure`]). Its fallback clock keeps running,
            // because the firmware's is a wall clock and nothing here
            // reset it.
            if round < boards[i].busy_until {
                boards[i].strict_rounds += 1;
                continue;
            }
            // The quiet spec suspends the clock while ANY link is live
            // (part 2's firmware reset it on every scan pass that found
            // a live connection); an outgoing link already stopped the
            // scan above, so incoming links are what decides here.
            let suspended = spec == FallbackSpec::Quiet && !boards[i].incoming.is_empty();
            let wants_fallback = spec != FallbackSpec::Off
                && !suspended
                && boards[i].strict_rounds >= FALLBACK_AFTER_ROUNDS;
            // #412 part 2: the ledger's pause. It only ever takes a
            // fallback dial away — a strict verdict is the #375 guarantee
            // and the ledger never touches one — so it is read exactly
            // where the mode would have flipped.
            let held = ledger.keeps()
                && wants_fallback
                && boards[i].ledger.fallback_held(u64::from(round) * ROUND_MS);
            let mode = if wants_fallback && !held {
                ScanMode::Fallback
            } else {
                ScanMode::Strict
            };
            let candidates =
                window_offers(i, mode, round, choice, &boards, &churners, &static_peers);
            if candidates.is_empty() {
                // What the pause cost, counted where it was paid: the
                // strict window is empty, so this board dials nothing
                // this round, and the fallback window it was not allowed
                // to open would have offered something. `solo` is the
                // window #412 part 2 exists for — one with no board in it
                // at all, which no candidate ORDER can defend.
                if held {
                    let would = window_offers(
                        i,
                        ScanMode::Fallback,
                        round,
                        choice,
                        &boards,
                        &churners,
                        &static_peers,
                    );
                    if !would.is_empty() {
                        let solo = !would.iter().any(|&(p, _, _, _)| p < churn_base);
                        tally.held_windows += 1;
                        tally.held_windows_solo += usize::from(solo);
                        if i == top_board {
                            tally.held_windows_top += 1;
                            tally.held_windows_top_solo += usize::from(solo);
                        }
                    }
                }
                if suspended {
                    boards[i].strict_rounds = 0;
                } else {
                    boards[i].strict_rounds += 1;
                }
                continue;
            }
            let target = match choice {
                // The pre-window firmware dialled whichever eligible
                // PDU it saw first, so the pick is arbitrary: seeded
                // random.
                TargetChoice::FirstSeen => {
                    candidates[(next_rand(&mut rng) as usize) % candidates.len()].0
                }
                // One round IS one collected window here: every
                // eligible advertiser was heard, the table chooses.
                // `LowestEligible` replays item 2 by having nobody
                // advertise a count; `MostFreeSlots` is the shipped
                // policy, every board stating its free slots; the last
                // two are #412 steady state 2's candidate keys, which
                // [`elect`] builds out of the same table.
                _ => {
                    // The order the advertising PDUs arrive in. It is
                    // the candidate order for every row that does not
                    // read it, and a seeded shuffle for the ones that
                    // do. The shuffle has a stream of ITS OWN, a
                    // third one: drawing from the main stream would
                    // move the arrival and scan orders, and drawing
                    // from the churn stream would give this row a
                    // different phone from every other row. Both would
                    // make the comparison between rows something other
                    // than a comparison of policies.
                    let mut offers = candidates.clone();
                    if choice.reads_arrival_order() {
                        for i in (1..offers.len()).rev() {
                            offers.swap(i, (next_rand(&mut offer_rng) as usize) % (i + 1));
                        }
                    }
                    elect(choice, &offers, &mut tally)
                }
            };
            // The dial. The firmware logs `BLE_CENTRAL_CONNECT` here and
            // does not yet know the outcome.
            tally.dials += 1;
            // The verdict this candidate was elected under: the only
            // thing that decides whether a dial that never connects
            // condemns its address (`columba.rs:2127`).
            let (target_addr, decision) = candidates
                .iter()
                .find(|&&(peer, _, _, _)| peer == target)
                .map(|&(_, addr, decision, _)| (addr, decision))
                .expect("the elected candidate was one of the offers");
            let strict_verdict = decision != ConnectDecision::InitiateFallback;
            // #412, the instrument's first limit: the dial that does not
            // become a link. A rotating peer's verdict belongs to its
            // current ADDRESS and was drawn at the rotation; a static
            // one's is drawn here, per dial.
            let rotating = (churn_base..static_base).contains(&target);
            let fails = failure.fires()
                && if rotating {
                    !churners[target - churn_base].reachable
                } else {
                    ConnectFailure::hits(failure.static_fail, &mut fail_rng)
                };
            if fails {
                let at_connect_rate = if rotating {
                    failure.rotating_at_connect
                } else {
                    failure.static_at_connect
                };
                let at_connect = ConnectFailure::hits(at_connect_rate, &mut fail_rng);
                tally.failed += 1;
                tally.failed_churn += usize::from(rotating);
                tally.failed_at_connect += usize::from(at_connect);
                if at_connect {
                    // `central::connect` timed out: no connection ever
                    // existed, so `conn_link_up` did not fire and the
                    // strict clock keeps whatever it had — a FALLBACK
                    // dial that fails here leaves the board stranded and
                    // in fallback, which is what `columba.rs:2598` says
                    // in as many words. The address is condemned only
                    // for a fallback verdict (#375 §0).
                    if strict_verdict {
                        boards[i].strict_rounds = 0;
                    } else {
                        boards[i].note_dead_end(target_addr, round);
                    }
                    boards[i].busy_until = round + FAIL_CONNECT_ROUNDS;
                } else {
                    // Past the connect: `conn_link_up` fired and reset
                    // the clock, and no stage past it writes a dead end
                    // at all — which is why an unreachable address is
                    // dialled a median six times before it rotates away.
                    boards[i].strict_rounds = 0;
                    boards[i].busy_until = round + FAIL_POST_ROUNDS;
                }
                // The ledger sees nothing: it is keyed by identity and a
                // dial that never read the Identity characteristic has
                // none (326's decision (i)).
                continue;
            }
            // From here on the connection exists, so the strict phase
            // restarts in either outcome (the firmware's
            // `conn_link_up`), and the identity read decides whether
            // anything was gained by it.
            boards[i].strict_rounds = 0;
            // The steady-state denominator: this board has held an
            // outgoing link before, so the slot it is spending now is
            // one that was freed.
            if boards[i].held_outgoing {
                let churned = (churn_base..static_base).contains(&target);
                let static_peer = target >= static_base;
                tally.refill_dials += 1;
                if churned {
                    tally.refill_dials_churn += 1;
                }
                if static_peer {
                    tally.refill_dials_static += 1;
                }
                if i == top_board {
                    tally.refill_dials_top += 1;
                    if static_peer {
                        tally.refill_dials_top_static += 1;
                    }
                    if churned {
                        tally.refill_dials_top_churn += 1;
                        if !candidates.iter().any(|&(p, _, _, _)| p < churn_base) {
                            tally.refill_dials_top_churn_solo += 1;
                        }
                    }
                }
            }
            if target >= static_base {
                // The rig's fourth kind. No identity check is reachable
                // here: its address never changes, so a link it already
                // holds with us was excluded by the §4.5 rule before the
                // dial. The duplicate is what a ROTATION makes.
                let own_addr = boards[i].addr;
                let dies_at = mortality
                    .kills()
                    .then(|| round + mortality.draw_lifetime(&mut death_rng));
                let peer = &mut static_peers[target - static_base];
                boards[i].outgoing = Some(Link {
                    peer: target,
                    addr: peer.addr,
                    formed: round,
                    silent_since: None,
                    dies_at,
                });
                peer.incoming.push(Link {
                    peer: i,
                    addr: own_addr,
                    formed: round,
                    silent_since: None,
                    dies_at: None,
                });
                peer.strict_rounds = 0;
                boards[i].held_outgoing = true;
                tally.static_links += 1;
                any_link = true;
                continue;
            }
            if target < churn_base {
                let (addr, own_addr) = (boards[target].addr, boards[i].addr);
                // The lifetime is drawn once, here, and only for a
                // board-to-board link: a link to a churning peer ends at
                // that peer's next rotation, a measured period.
                let dies_at = mortality
                    .kills()
                    .then(|| round + mortality.draw_lifetime(&mut death_rng));
                boards[i].outgoing = Some(Link {
                    peer: target,
                    addr,
                    formed: round,
                    silent_since: None,
                    dies_at,
                });
                boards[target].incoming.push(Link {
                    peer: i,
                    addr: own_addr,
                    formed: round,
                    silent_since: None,
                    // The peripheral end holds no clock of its own: the
                    // central's `dies_at` removes both entries at once.
                    dies_at: None,
                });
                boards[target].strict_rounds = 0;
                boards[i].held_outgoing = true;
                tally.board_links += 1;
                any_link = true;
                continue;
            }
            let churner = &mut churners[target - churn_base];
            // Post-connect: the Identity characteristic. A live link to
            // this identity under an older address is the #412 case.
            if let Some((origin, old)) = boards[i].link_with(target) {
                debug_assert_eq!(origin, Origin::Incoming, "a busy central does not scan");
                let silence = u64::from(round - old.silent_since.unwrap_or(round)) * ROUND_MS;
                let verdict = judge_duplicate(
                    silence,
                    origin,
                    Origin::Outgoing,
                    Some(SIM_USABLE_MTU),
                    SIM_USABLE_MTU,
                    &boards[i].identity,
                    &churner.identity,
                );
                match verdict {
                    DupVerdict::KeepNew(_) => {
                        boards[i].incoming.retain(|l| l.peer != target);
                        churner.links.retain(|&b| b != i);
                    }
                    DupVerdict::KeepOld(_) | DupVerdict::Wait => {
                        // Refused. The address is backed off — and the
                        // peer's next rotation walks straight past it.
                        // The IDENTITY is what the ledger writes down
                        // instead (#412 part 2, and the outcome part 3
                        // named): a dial that was paid for in full and
                        // bought no session at all is wasted whatever
                        // address it was spent on.
                        tally.refused += 1;
                        let addr = churner.addr;
                        let identity = churner.identity;
                        boards[i].note_dead_end(addr, round);
                        boards[i].note_dial_outcome(ledger, &identity, None, round);
                        continue;
                    }
                }
            }
            boards[i].outgoing = Some(Link {
                peer: target,
                addr: churner.addr,
                formed: round,
                silent_since: None,
                dies_at: None,
            });
            boards[i].held_outgoing = true;
            tally.churn_links += 1;
            any_link = true;
        }

        // The churning peer's own scan pass: it is central-capable and
        // its address is always the lower one, so the v2.2 sort has it
        // dial every board it can reach. One dial per round — it has
        // one radio — and the lowest-addressed eligible board, the same
        // choice the shared window makes.
        for (k, churner) in churners.iter_mut().enumerate() {
            if !churn.dials || churner.links.len() >= CHURN_CENTRAL_LINKS {
                continue;
            }
            let churner_addr = churner.addr;
            let target = boards
                .iter()
                .enumerate()
                .filter(|(b, board)| {
                    board.arrived
                        && board.incoming.len() < PERIPH_SLOTS
                        && !board.addr_linked(churner_addr)
                        && !churner.links.contains(b)
                        && should_initiate(0, churner_addr, Some(0), board.addr, ScanMode::Strict)
                            .initiate()
                })
                .min_by_key(|(_, board)| board.addr)
                .map(|(b, _)| b);
            let Some(b) = target else { continue };
            // The board's own identity check on the incoming side: the
            // mirror of the one above, same rule, roles swapped.
            if let Some((origin, old)) = boards[b].link_with(n + k) {
                let silence = u64::from(round - old.silent_since.unwrap_or(round)) * ROUND_MS;
                let verdict = judge_duplicate(
                    silence,
                    origin,
                    Origin::Incoming,
                    Some(SIM_USABLE_MTU),
                    SIM_USABLE_MTU,
                    &boards[b].identity,
                    &churner.identity,
                );
                match verdict {
                    DupVerdict::KeepNew(_) => match origin {
                        Origin::Outgoing => {
                            let link = boards[b].outgoing.take().expect("origin says outgoing");
                            if link.useful_ms(round) < USEFUL_SESSION_MS {
                                tally.short += 1;
                            }
                            // The peer's own dial displaced the link OUR
                            // dial paid for, so that dial's outcome is
                            // known here too (#412 part 2).
                            let identity = churner.identity;
                            boards[b].note_dial_outcome(
                                ledger,
                                &identity,
                                Some(link.useful_ms(round)),
                                round,
                            );
                            boards[b].strict_rounds = 0;
                        }
                        Origin::Incoming => boards[b].incoming.retain(|l| l.peer != n + k),
                    },
                    // Our old link keeps the peer: the phone's dial is
                    // refused. Nothing of ours was spent on it.
                    DupVerdict::KeepOld(_) | DupVerdict::Wait => continue,
                }
            }
            boards[b].incoming.push(Link {
                peer: n + k,
                addr: churner_addr,
                formed: round,
                silent_since: None,
                dies_at: None,
            });
            boards[b].strict_rounds = 0;
            churner.links.push(b);
        }

        // The static peer's own scan pass (#412 steady state 2). It is
        // the board it is: ONE outgoing link at a time, the same
        // [`should_initiate`] rule, the same window and the same
        // fallback clock. At the rig's own address order it is the
        // lowest in the room, so every board is a strict candidate for
        // it and the clock never runs — which is why the capture shows
        // 84 links it initiated and not one board that strictly elected
        // it. It dials boards only: the captures place all 84 at the
        // three boards, and they cannot see a link of its own to the
        // phone, so the model does not invent one.
        for (k, peer) in static_peers.iter_mut().enumerate() {
            if !statics.dials || peer.outgoing.is_some() {
                continue;
            }
            let own_addr = peer.addr;
            let mode = if spec != FallbackSpec::Off && peer.strict_rounds >= FALLBACK_AFTER_ROUNDS {
                ScanMode::Fallback
            } else {
                ScanMode::Strict
            };
            let offers: Vec<(usize, u64, ConnectDecision, Option<u8>)> = (0..n)
                .filter_map(|b| {
                    if !boards[b].arrived
                        || boards[b].incoming.len() >= PERIPH_SLOTS
                        || boards[b].addr_linked(own_addr)
                    {
                        return None;
                    }
                    let free = choice.advertises_slots().then(|| {
                        u8::try_from(PERIPH_SLOTS - boards[b].incoming.len())
                            .expect("slots fit a byte")
                    });
                    let decision = should_initiate(0, own_addr, Some(0), boards[b].addr, mode);
                    decision
                        .initiate()
                        .then_some((b, boards[b].addr, decision, free))
                })
                .collect();
            if offers.is_empty() {
                peer.strict_rounds += 1;
                continue;
            }
            // Its dials are not in the boards' ledger — the room did not
            // pay for them — so the tier census must not count them
            // either; the window it opens is its own.
            let mut side_ledger = Tally::default();
            let b = elect(choice, &offers, &mut side_ledger);
            let dies_at = mortality
                .kills()
                .then(|| round + mortality.draw_lifetime(&mut death_rng));
            peer.outgoing = Some(Link {
                peer: b,
                addr: boards[b].addr,
                formed: round,
                silent_since: None,
                dies_at,
            });
            peer.strict_rounds = 0;
            boards[b].incoming.push(Link {
                peer: static_base + k,
                addr: own_addr,
                formed: round,
                silent_since: None,
                dies_at: None,
            });
            boards[b].strict_rounds = 0;
            any_link = true;
        }

        linkless_streak = if any_link { 0 } else { linkless_streak + 1 };
        // Quiescent: everyone has arrived and even the boards that
        // reached fallback during the streak found nobody. Visibility
        // only changes when a link forms (a quiet-suspended board's
        // links never drop here), so nothing changes hereafter. With a
        // churning peer nothing is ever quiescent — rotations keep
        // arriving — so the run goes to the horizon instead.
        if churn.peers == 0
            && statics.peers == 0
            && !mortality.kills()
            && !failure.fires()
            && (round as usize) >= n
            && linkless_streak > FALLBACK_AFTER_ROUNDS
        {
            break;
        }
    }

    // Settle the links still standing at the horizon by the same rule.
    // Under `Mortality::IMMORTAL` only a churn session can be short, and
    // the condition is exactly what it was; with mortality on, every
    // board link is a session that ends too, so a young one standing at
    // the horizon is short like any other.
    // A standing link to a STATIC peer is not a churn session: nothing
    // rotates it away, so it is short only if the mortality row says
    // every session ends.
    for board in &boards {
        if let Some(link) = board.outgoing {
            let churning = (churn_base..static_base).contains(&link.peer);
            if (churning || mortality.kills()) && link.useful_ms(horizon) < USEFUL_SESSION_MS {
                tally.short += 1;
            }
        }
    }

    // The churn rule held: no pair ever holds two links, and every dial
    // ended in exactly one of the three outcomes the ledger counts.
    for a in 0..n {
        if let Some(link) = boards[a].outgoing {
            if link.peer < n {
                assert_ne!(
                    boards[link.peer].outgoing.map(|l| l.peer),
                    Some(a),
                    "duplicate pair link"
                );
            }
        }
        assert!(boards[a].incoming.len() <= PERIPH_SLOTS);
        // Both ends of a board-to-board link exist or neither does. A
        // death that freed the central's outgoing slot but left the
        // peripheral's incoming one occupied would be invisible in every
        // column — a board with three stale incoming links still forms
        // outgoing ones and still shows up connected — so the invariant
        // is checked on every order rather than in one test.
        for link in &boards[a].incoming {
            if link.peer < n {
                assert_eq!(
                    boards[link.peer].outgoing.map(|l| l.peer),
                    Some(a),
                    "board {a} holds an incoming link from {} that {} does not hold",
                    link.peer,
                    link.peer
                );
            }
        }
    }
    // The static peer's own bookkeeping: its slot bound holds, and both
    // ends of every link it holds exist. A one-sided static link would
    // silently take a board's incoming slot out of the room forever.
    for (k, peer) in static_peers.iter().enumerate() {
        assert!(peer.incoming.len() <= PERIPH_SLOTS);
        for link in &peer.incoming {
            assert_eq!(
                boards[link.peer].outgoing.map(|l| l.peer),
                Some(static_base + k),
                "the static peer holds an incoming link from board {} that the board does not",
                link.peer
            );
        }
        if let Some(link) = peer.outgoing {
            assert!(
                boards[link.peer]
                    .incoming
                    .iter()
                    .any(|l| l.peer == static_base + k),
                "the static peer holds an outgoing link to board {} that the board does not",
                link.peer
            );
        }
    }
    assert_eq!(
        tally.dials,
        tally.board_links + tally.churn_links + tally.static_links + tally.refused + tally.failed,
        "a dial went uncounted"
    );
    Sim { boards, tally }
}

/// Connectivity over the undirected board-to-board link graph.
fn is_connected(boards: &[Board]) -> bool {
    let n = boards.len();
    let mut seen = vec![false; n];
    let mut queue = vec![0usize];
    seen[0] = true;
    while let Some(at) = queue.pop() {
        for (next, seen_next) in seen.iter_mut().enumerate() {
            if !*seen_next && linked(boards, at, next) {
                *seen_next = true;
                queue.push(next);
            }
        }
    }
    seen.iter().all(|&s| s)
}

/// One configuration's rates over [`ORDERS`] seeded orders.
struct Outcome {
    /// Orders whose final board graph is not one connected component.
    disconnected: usize,
    /// Orders where some board ended with no BLE link at all.
    linkless: usize,
    /// Boards, summed over all orders, that ended with every incoming
    /// slot spent. A saturated board is where the real refusals happen
    /// (#375 item 3: several searchers race into the same last slot),
    /// so this is the load-spread the slot preference is FOR — the
    /// connectivity columns cannot show it, because the sim reads a
    /// peer's capacity directly instead of dialling and being refused.
    saturated: usize,
    /// Boards, summed over all orders, that ended with no link to
    /// another BOARD. #412's second number seen per board: a board
    /// whose only link is the phone is not on the mesh.
    boardless: usize,
    /// Board-to-board links formed, summed over all orders — #412's
    /// second number. These never end here, so the count is both
    /// "formed" and "standing at the end".
    board_links: usize,
    /// Every dial that reached the identity read, summed.
    dials: usize,
    /// Dials that landed on a churning peer and formed a link, summed.
    /// With `dials` it gives the number the rig captures directly:
    /// `feld-t114` made 7 outgoing links in two days and all 7 were the
    /// phone; `t114-boot` 12 of 22.
    churn_links: usize,
    /// Dials that landed on a static peer and formed a link, summed —
    /// the volume behind the `stat%` column, and on the rig the 42 links
    /// `feld-t114` spent on the solar node.
    static_links: usize,
    /// Dials that produced a link that lasted at least
    /// [`USEFUL_SESSION_MS`], summed — #412's third number is
    /// `dials / useful`.
    useful: usize,
    /// Of the spent ones: refused as a live duplicate identity.
    refused: usize,
    /// Dials that did not connect at all, summed ([`ConnectFailure`]),
    /// the ones among them aimed at a churning peer, and the ones that
    /// failed before the connection existed. Zero under
    /// [`ConnectFailure::NONE`].
    failed: usize,
    failed_churn: usize,
    failed_at_connect: usize,
    /// Of the spent ones: a session below the churn threshold.
    short: usize,
    /// Board-to-board links that reached their drawn lifetime, summed —
    /// zero without [`Mortality`], and the model's positive control with
    /// it.
    deaths: usize,
    /// Dials that spent a FREED outgoing slot, summed: the steady-state
    /// denominator (#412 number 1).
    refill_dials: usize,
    /// Of those, the ones aimed at a churning peer.
    refill_dials_churn: usize,
    /// And at a static peer (#412 steady state 2).
    refill_dials_static: usize,
    /// The same pair for the highest-addressed board alone — the board
    /// the rig's 7 of 7 is about — and how many of those dials had no
    /// board among the permitted candidates at all.
    refill_dials_top: usize,
    refill_dials_top_churn: usize,
    refill_dials_top_static: usize,
    refill_dials_top_churn_solo: usize,
    /// Window offers and 303's tier among them, summed.
    offers: usize,
    offers_cannot_dial: usize,
    /// Windows the [`Ledger`] held shut, summed, and the ones among them
    /// that had no board on offer at all — the solo window #412 part 2
    /// exists for. Zero under [`Ledger::NONE`].
    held_windows: usize,
    held_windows_solo: usize,
    /// The same pair for the highest-addressed board alone.
    held_windows_top: usize,
    held_windows_top_solo: usize,
}

impl Outcome {
    /// What share of every dial the room made was aimed at a churning
    /// peer, refusals included — the rig's own reading of #412, and the
    /// one number here that does not depend on how "useful" is defined.
    fn share_spent_on_churn(&self) -> f64 {
        if self.dials == 0 {
            return 0.0;
        }
        (self.churn_links + self.refused) as f64 * 100.0 / self.dials as f64
    }

    /// What share of the dials that spent a FREED outgoing slot went to
    /// a churning peer — the rig's steady-state reading of #412, where
    /// `feld-t114` freed its slot seven times and the phone took all
    /// seven. `None` when no slot was ever freed, which is what the
    /// formation phase is: a statement, not a zero.
    fn share_of_freed_slots_to_churn(&self) -> Option<f64> {
        (self.refill_dials > 0)
            .then(|| self.refill_dials_churn as f64 * 100.0 / self.refill_dials as f64)
    }

    /// The same share for the highest-addressed board alone — the one
    /// reading that IS comparable to `feld-t114`'s 7 of 7, because that
    /// board is the one the room's sort leaves without a strict board
    /// candidate.
    fn share_of_top_freed_slots_to_churn(&self) -> Option<f64> {
        (self.refill_dials_top > 0)
            .then(|| self.refill_dials_top_churn as f64 * 100.0 / self.refill_dials_top as f64)
    }

    /// The column #412 steady state 2 asks for: of the top board's dials
    /// that spent a freed slot, the share that went to a STATIC peer.
    /// On the rig this is the number that emptied the phone's column —
    /// `feld-t114` spent 25 of 25 there after 2026-09-25.
    fn share_of_top_freed_slots_to_static(&self) -> Option<f64> {
        (self.refill_dials_top > 0)
            .then(|| self.refill_dials_top_static as f64 * 100.0 / self.refill_dials_top as f64)
    }

    /// The same for the room as a whole.
    fn share_of_freed_slots_to_static(&self) -> Option<f64> {
        (self.refill_dials > 0)
            .then(|| self.refill_dials_static as f64 * 100.0 / self.refill_dials as f64)
    }

    /// Of the top board's freed slots that went to a churning peer: what
    /// share had no board on offer at all. Where this is 100 %, no
    /// candidate ORDER can defend the slot — there is nothing to order.
    fn share_of_top_churn_dials_with_no_board_on_offer(&self) -> Option<f64> {
        (self.refill_dials_top_churn > 0).then(|| {
            self.refill_dials_top_churn_solo as f64 * 100.0 / self.refill_dials_top_churn as f64
        })
    }

    /// Of the windows the ledger held shut, the share that had no board
    /// on offer at all: how much of the pause landed on the window it is
    /// FOR. `None` when the ledger never held a window shut, which is
    /// what [`Ledger::NONE`] is and a statement rather than a zero.
    fn share_of_held_windows_solo(&self) -> Option<f64> {
        (self.held_windows > 0)
            .then(|| self.held_windows_solo as f64 * 100.0 / self.held_windows as f64)
    }

    /// What share of every dial the room made never became a link at
    /// all ([`ConnectFailure`]) — the column the captures read directly:
    /// `feld-t114` 2678 of 2729, `t114-boot` 48 of 164, `feld-pocket`
    /// 49 of 109. `None` when the room made no dial.
    fn share_of_dials_that_failed(&self) -> Option<f64> {
        (self.dials > 0).then(|| self.failed as f64 * 100.0 / self.dials as f64)
    }

    /// Dials per board-to-board link — #412's third number read against
    /// the Leitstern instead of against link lifetime. A link to a
    /// phone is useful TO THE PHONE; it is not a link the mesh gained.
    fn dials_per_board_link(&self) -> Option<f64> {
        (self.board_links > 0).then(|| self.dials as f64 / self.board_links as f64)
    }

    /// #412's third number. `None` when nothing useful was dialled at
    /// all, which is a statement of its own and not a zero.
    fn dials_per_useful(&self) -> Option<f64> {
        (self.useful > 0).then(|| self.dials as f64 / self.useful as f64)
    }
}

// Eight, for the reason [`run_sim`]'s comment gives: this is that list
// minus the seed, and every one of them is a column some table varies.
#[allow(clippy::too_many_arguments)]
fn measure(
    n: usize,
    spec: FallbackSpec,
    choice: TargetChoice,
    churn: Churn,
    mortality: Mortality,
    statics: Statics,
    ledger: Ledger,
    failure: ConnectFailure,
) -> Outcome {
    let mut outcome = Outcome {
        disconnected: 0,
        linkless: 0,
        saturated: 0,
        boardless: 0,
        board_links: 0,
        dials: 0,
        churn_links: 0,
        static_links: 0,
        useful: 0,
        refused: 0,
        failed: 0,
        failed_churn: 0,
        failed_at_connect: 0,
        short: 0,
        deaths: 0,
        refill_dials: 0,
        refill_dials_churn: 0,
        refill_dials_static: 0,
        refill_dials_top: 0,
        refill_dials_top_churn: 0,
        refill_dials_top_static: 0,
        refill_dials_top_churn_solo: 0,
        offers: 0,
        offers_cannot_dial: 0,
        held_windows: 0,
        held_windows_solo: 0,
        held_windows_top: 0,
        held_windows_top_solo: 0,
    };
    for seed in 0..ORDERS {
        let sim = run_sim(
            n,
            0xB1E5_0000 + seed,
            spec,
            choice,
            churn,
            mortality,
            statics,
            ledger,
            failure,
        );
        let boards = &sim.boards;
        if !is_connected(boards) {
            outcome.disconnected += 1;
        }
        if boards
            .iter()
            .any(|b| b.outgoing.is_none() && b.incoming.is_empty())
        {
            outcome.linkless += 1;
        }
        outcome.saturated += boards
            .iter()
            .filter(|b| b.incoming.len() >= PERIPH_SLOTS)
            .count();
        outcome.boardless += (0..n)
            .filter(|&i| !(0..n).any(|j| j != i && linked(boards, i, j)))
            .count();
        outcome.board_links += sim.tally.board_links;
        outcome.dials += sim.tally.dials;
        outcome.churn_links += sim.tally.churn_links;
        outcome.static_links += sim.tally.static_links;
        outcome.useful += sim.tally.useful();
        outcome.refused += sim.tally.refused;
        outcome.failed += sim.tally.failed;
        outcome.failed_churn += sim.tally.failed_churn;
        outcome.failed_at_connect += sim.tally.failed_at_connect;
        outcome.short += sim.tally.short;
        outcome.deaths += sim.tally.deaths;
        outcome.refill_dials += sim.tally.refill_dials;
        outcome.refill_dials_churn += sim.tally.refill_dials_churn;
        outcome.refill_dials_static += sim.tally.refill_dials_static;
        outcome.refill_dials_top += sim.tally.refill_dials_top;
        outcome.refill_dials_top_churn += sim.tally.refill_dials_top_churn;
        outcome.refill_dials_top_static += sim.tally.refill_dials_top_static;
        outcome.refill_dials_top_churn_solo += sim.tally.refill_dials_top_churn_solo;
        outcome.offers += sim.tally.offers;
        outcome.offers_cannot_dial += sim.tally.offers_cannot_dial;
        outcome.held_windows += sim.tally.held_windows;
        outcome.held_windows_solo += sim.tally.held_windows_solo;
        outcome.held_windows_top += sim.tally.held_windows_top;
        outcome.held_windows_top_solo += sim.tally.held_windows_top_solo;
    }
    outcome
}

/// Every policy the file compares, as one label table.
const CONFIGS: [(FallbackSpec, TargetChoice, &str); 8] = [
    (FallbackSpec::Off, TargetChoice::FirstSeen, "strict"),
    (FallbackSpec::Eager, TargetChoice::FirstSeen, "eager/first"),
    (
        FallbackSpec::Eager,
        TargetChoice::LowestEligible,
        "eager/lowest",
    ),
    (FallbackSpec::Quiet, TargetChoice::FirstSeen, "quiet/first"),
    (
        FallbackSpec::Quiet,
        TargetChoice::LowestEligible,
        "quiet/lowest",
    ),
    (
        FallbackSpec::Eager,
        TargetChoice::MostFreeSlots,
        "eager/mostfree",
    ),
    (
        FallbackSpec::Eager,
        TargetChoice::FallbackFirstHeard,
        "eager/firstheard",
    ),
    (
        FallbackSpec::Eager,
        TargetChoice::RotatingLast,
        "eager/rotatinglast",
    ),
];

/// A measured quantity as the tables print it: two decimals, or `-`
/// when it is undefined (no useful link, no board link) — which is a
/// statement of its own and not a zero.
struct Ratio(Option<f64>);

impl std::fmt::Display for Ratio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(ratio) => write!(f, "{ratio:.2}"),
            None => f.write_str("-"),
        }
    }
}

/// The batch's two-spec table (item 1): both fallback specs, with and
/// without the lowest-eligible window, at both sizes, plus the strict
/// baseline — printed for the record (`--nocapture`), with the
/// load-bearing cells asserted:
///
/// - eager/lowest yields 0/1000 disconnected orders at both sizes —
///   the saturated-cycle lock is a first-seen artefact and the window
///   removes it entirely;
/// - the quiet spec's cost against eager/lowest is exactly the pinned
///   28 and 78 orders (the module docs say why, and why part 3 shipped
///   eager over it); the seed stream is fixed, so equality, like the
///   calibration cells;
/// - no fallback configuration ever leaves a board linkless — #375's
///   headline failure stays gone under every spec;
/// - the calibration cells still match the published measurements
///   (eager/first: 2 and 48), so the instrument itself has not moved.
///
/// Since #412 this table is also the churn model's control: it runs at
/// [`Churn::NONE`], and every number in it is what it was before the
/// churning peer existed. A pinned cell that moves here means the
/// addition changed the thing it was meant to observe.
#[test]
fn the_two_spec_table_the_window_closes_the_lock_and_quiet_costs_a_pinned_rest() {
    let mut rates = std::collections::HashMap::new();
    println!(
        "spec/choice     n=10 disc/linkless/sat   n=20 disc/linkless/sat   (per {ORDERS} orders)"
    );
    for (spec, choice, label) in CONFIGS {
        let at10 = measure(
            10,
            spec,
            choice,
            Churn::NONE,
            Mortality::IMMORTAL,
            Statics::NONE,
            Ledger::NONE,
            ConnectFailure::NONE,
        );
        let at20 = measure(
            20,
            spec,
            choice,
            Churn::NONE,
            Mortality::IMMORTAL,
            Statics::NONE,
            Ledger::NONE,
            ConnectFailure::NONE,
        );
        println!(
            "{label:<15} {:>4} / {:<4} / {:<8} {:>4} / {:<4} / {:<8}",
            at10.disconnected,
            at10.linkless,
            at10.saturated,
            at20.disconnected,
            at20.linkless,
            at20.saturated
        );
        rates.insert(label, (at10, at20));
    }

    let (at10, at20) = &rates["eager/lowest"];
    assert_eq!(
        (at10.disconnected, at20.disconnected),
        (0, 0),
        "eager/lowest: the lowest-eligible window must close the saturation lock"
    );
    let (at10, at20) = &rates["eager/mostfree"];
    assert_eq!(
        (at10.disconnected, at20.disconnected),
        (0, 0),
        "eager/mostfree: the slot preference regressed convergence"
    );
    assert_eq!(
        (at10.saturated, at20.saturated),
        (498, 914),
        "eager/mostfree saturation moved: the documented spread is stale, re-measure"
    );
    let spread = (
        rates["eager/lowest"].0.saturated,
        rates["eager/lowest"].1.saturated,
    );
    // A direction, not a re-statement of the pinned pair: at least a
    // 40 % cut at both sizes (measured 49 % and 64 %), so the claim
    // survives a re-seeded instrument while a lost preference does not.
    assert!(
        at10.saturated * 10 <= spread.0 * 6 && at20.saturated * 10 <= spread.1 * 6,
        "the slot preference must cut saturated boards by at least 40 % \
         (mostfree {} / {}, lowest {} / {})",
        at10.saturated,
        at20.saturated,
        spread.0,
        spread.1
    );
    let (at10, at20) = &rates["quiet/lowest"];
    assert_eq!(
        (at10.disconnected, at20.disconnected),
        (28, 78),
        "quiet/lowest moved: the quiet spec's documented cost is stale, re-measure"
    );
    // #412's shipped order changes the fallback class's last term for
    // addresses that are redrawn, and a room of boards has none: every
    // column of the empty room must therefore be the SAME NUMBER as
    // the order it replaced, not merely as good. This is the claim
    // that makes the change safe to ship, and it is an equality.
    for (shipped, replaced) in [
        (&rates["eager/rotatinglast"].0, &rates["eager/mostfree"].0),
        (&rates["eager/rotatinglast"].1, &rates["eager/mostfree"].1),
    ] {
        assert_eq!(
            (
                shipped.disconnected,
                shipped.linkless,
                shipped.saturated,
                shipped.boardless,
                shipped.board_links,
                shipped.dials
            ),
            (
                replaced.disconnected,
                replaced.linkless,
                replaced.saturated,
                replaced.boardless,
                replaced.board_links,
                replaced.dials
            ),
            "the rotating-last order moved a number in a room that has no rotating address"
        );
    }
    // And the order it did NOT ship, for the record: dropping the
    // address term for every candidate gives back most of the
    // saturated-cycle lock the window closed.
    let (at10, at20) = &rates["eager/firstheard"];
    assert_eq!(
        (at10.disconnected, at20.disconnected),
        (2, 74),
        "eager/firstheard moved: the empty-room cost of the first-heard order is stale"
    );
    for label in [
        "eager/first",
        "eager/lowest",
        "eager/mostfree",
        "quiet/first",
        "quiet/lowest",
    ] {
        let (at10, at20) = &rates[label];
        assert_eq!(
            (at10.linkless, at20.linkless),
            (0, 0),
            "{label}: a fallback spec left some board with no BLE link at all"
        );
    }
    let (cal10, cal20) = &rates["eager/first"];
    assert_eq!(
        (cal10.disconnected, cal20.disconnected),
        (2, 48),
        "the calibration cells moved: the instrument changed, re-measure everything"
    );
    // With nobody churning, nothing a board dials ever ends: every dial
    // is a board-to-board link that lasts, the ledger reads exactly
    // 1.00 and the spent columns are empty. This is the ledger's own
    // control — waste at churn 0 would be waste the accounting invented.
    for (_, _, label) in CONFIGS {
        let (at10, at20) = &rates[label];
        for outcome in [at10, at20] {
            assert_eq!(
                (outcome.refused, outcome.short, outcome.churn_links),
                (0, 0, 0),
                "{label}: a dial was spent on churn with nobody churning"
            );
            assert_eq!(
                (outcome.dials, outcome.board_links),
                (outcome.useful, outcome.useful),
                "{label}: every dial at churn 0 is a board link that lasts"
            );
        }
    }
}

/// #412: the same policies with churning peers in the room — one
/// identity each, a new address every 48 s, always advertising,
/// central-capable, sessions ending at the next rotation. The three
/// numbers the issue asks for, per policy and per board count, at 0, 1
/// and 2 churning peers, plus the two readings that make the third one
/// legible: what share of every dial the room aimed at a churning peer
/// (the rig's own reading — `feld-t114` 7 of 7, `t114-boot` 12 of 22),
/// and dials per board-to-board link.
///
/// The interpretation is in the module docs. What this test holds on to
/// is the mechanism, so that a fix has to move it:
///
/// - **a churning peer takes the fallback dial, and takes it every
///   time**, because a resolvable private address is structurally below
///   every static-random board address (`peer.rs`) and the fallback
///   class is ordered by address. Nearly half of the room's dials end
///   at the phone;
/// - **the board graph pays for it**: from 0 disconnected orders per
///   1000 to a number that is not 0, and board-to-board links fall;
/// - **the window choice is no defence**: eager/mostfree, the shipped
///   policy, ends up where eager/first does — the preference orders
///   candidates INSIDE the fallback class, and the phone is first in it
///   whatever the boards advertise.
#[test]
fn a_churning_peer_takes_the_fallback_dial_and_the_board_graph_pays_for_it() {
    let mut rates = std::collections::HashMap::new();
    println!(
        "#412 churn table — per {ORDERS} orders; disc = split board graphs, boardless = boards"
    );
    println!("with no board link, bb = board-to-board links formed, %churn = share of dials aimed");
    println!("at a churning peer, d/bb = dials per board link, d/use = dials per link that lasted");
    println!(
        "churn spec/choice      n=10   disc boardless     bb %churn  d/bb d/use \
         | n=20   disc boardless     bb %churn  d/bb d/use"
    );
    for churn in [Churn::NONE, Churn::phones(1), Churn::phones(2)] {
        for (spec, choice, label) in CONFIGS {
            let at10 = measure(
                10,
                spec,
                choice,
                churn,
                Mortality::IMMORTAL,
                Statics::NONE,
                Ledger::NONE,
                ConnectFailure::NONE,
            );
            let at20 = measure(
                20,
                spec,
                choice,
                churn,
                Mortality::IMMORTAL,
                Statics::NONE,
                Ledger::NONE,
                ConnectFailure::NONE,
            );
            let cells = |outcome: &Outcome| {
                format!(
                    "{:>6} {:>9} {:>6} {:>5.0} {:>5} {:>5}",
                    outcome.disconnected,
                    outcome.boardless,
                    outcome.board_links,
                    outcome.share_spent_on_churn(),
                    Ratio(outcome.dials_per_board_link()),
                    Ratio(outcome.dials_per_useful()),
                )
            };
            println!(
                "{:>5} {label:<15} {} | {}",
                churn.peers,
                cells(&at10),
                cells(&at20)
            );
            rates.insert((churn.peers, label), (at10, at20));
        }
    }

    for (size, n) in [(0usize, 10usize), (1, 20)] {
        let pick = |peers: usize, label: &'static str| -> &Outcome {
            let (at10, at20) = &rates[&(peers, label)];
            if size == 0 {
                at10
            } else {
                at20
            }
        };
        let clean = pick(0, "eager/mostfree");
        let churned = pick(1, "eager/mostfree");
        assert_eq!(
            (clean.disconnected, clean.share_spent_on_churn() as u32),
            (0, 0),
            "n={n}: the churn-0 column is not the shipped policy's clean result"
        );
        // THE number, and the one the rig can be held against: the room
        // aims a third or more of every dial it makes at a peer that is
        // gone again in 48 s. The rig's three boards spent 20 of 37.
        assert!(
            churned.share_spent_on_churn() >= 25.0,
            "n={n}: one churning peer must take at least a quarter of the room's dials \
             (measured 42 % at n=10 and 28 % at n=20; got {:.0} %, {} of {} dials)",
            churned.share_spent_on_churn(),
            churned.churn_links + churned.refused,
            churned.dials
        );
        assert!(
            churned.disconnected > 0,
            "n={n}: the shipped policy stayed at 0 split graphs with a churning peer"
        );
        assert!(
            churned.board_links * 100 <= clean.board_links * 96,
            "n={n}: one churning peer must cost at least 4 % of the board-to-board links \
             (churned {}, clean {})",
            churned.board_links,
            clean.board_links
        );
        assert!(
            churned.refused > 0,
            "n={n}: no dial was ever refused as a live duplicate identity — that rotated-address \
             duplicate is the mechanism #412 names and the model lost it"
        );
        // The window choice is not merely no defence, it is the
        // mechanism: the fallback class is ordered by address and an
        // RPA is always the lowest one, so EVERY fallback dial of every
        // board goes to the phone. The pre-window firmware picked an
        // arbitrary eligible advertiser and therefore spread its
        // fallback dials — which is why first-seen, the policy #375
        // item 2 replaced, is the one that barely notices the churn.
        let churned_first = pick(1, "eager/first");
        assert!(
            churned_first.share_spent_on_churn() * 2.0 < churned.share_spent_on_churn(),
            "n={n}: first-seen no longer spreads its fallback dials away from the churning \
             peer (first-seen {:.0} %, window {:.0} %) — the window's cost is the whole \
             finding, re-measure before believing it went away",
            churned_first.share_spent_on_churn(),
            churned.share_spent_on_churn()
        );
        assert!(
            churned.board_links < churned_first.board_links,
            "n={n}: the address-ordered window kept as many board links as first-seen under \
             churn (window {}, first-seen {})",
            churned.board_links,
            churned_first.board_links
        );

        // What the shipped order does about it. The three numbers the
        // issue asks for, each against the order it replaced and in
        // the direction that matters, plus the one that says the
        // change is not an exclusion.
        let shipped = pick(1, "eager/rotatinglast");
        assert!(
            shipped.share_spent_on_churn() * 4.0 < churned.share_spent_on_churn(),
            "n={n}: the rotating-last order must cut the room's churn dials to well under a \
             quarter of what the address order spent (rotating-last {:.0} %, address {:.0} %)",
            shipped.share_spent_on_churn(),
            churned.share_spent_on_churn()
        );
        assert!(
            shipped.board_links * 100 >= clean.board_links * 99,
            "n={n}: the board-to-board links a churning peer cost must come back (shipped {}, \
             address order {}, empty room {})",
            shipped.board_links,
            churned.board_links,
            clean.board_links
        );
        assert!(
            shipped.disconnected * 2 <= churned.disconnected,
            "n={n}: the shipped order must at least halve the split board graphs a churning \
             peer causes (shipped {}, address order {})",
            shipped.disconnected,
            churned.disconnected
        );
        assert!(
            shipped.boardless <= churned.boardless,
            "n={n}: more boards ended with no board link under the shipped order ({} against {})",
            shipped.boardless,
            churned.boardless
        );
        // It is a preference, not an exclusion: with two churning
        // peers in the room some board still has nobody else to dial,
        // and dials them. A zero here would mean the order had become
        // a rule about who may be connected to at all.
        let two = pick(2, "eager/rotatinglast");
        assert!(
            two.churn_links > 0,
            "n={n}: the shipped order stopped dialling churning peers entirely — that is an \
             exclusion, and the fallback class must stay permitted"
        );
    }
}

/// The churn model's positive controls. The model is new, so each of
/// its mechanisms is shown firing once, in isolation, before the table
/// above may be read as a measurement — and the one effect that pulls
/// the OTHER way is pinned too, so nobody reads a churn row as good news.
#[test]
fn control_the_churn_model_is_a_parameter_and_every_mechanism_fires() {
    // 1. The round is the firmware's cycle and the periods are the
    //    capture's, so the model's clock is not free-floating.
    assert_eq!(ROUND_MS, 5_000, "the round is the firmware's retry cycle");
    assert_eq!(rounds(CHURN_ROTATE_MS), 9, "48 s at 5 s per round");
    // The existing defence cannot be the answer: an address is
    // condemned for longer than the peer keeps it.
    assert!(
        rounds(DEAD_END_TTL_MS) > rounds(CHURN_ROTATE_MS),
        "the dead-end TTL no longer outlives a rotation; the model's premise moved"
    );

    // 2. Zero churning peers is the pre-#412 simulation order by order,
    //    not just in aggregate.
    for n in [10usize, 20] {
        for seed in 0..50 {
            let sim = run_sim(
                n,
                0xB1E5_0000 + seed,
                FallbackSpec::Eager,
                TargetChoice::MostFreeSlots,
                Churn::NONE,
                Mortality::IMMORTAL,
                Statics::NONE,
                Ledger::NONE,
                ConnectFailure::NONE,
            );
            assert_eq!(sim.tally.dials, sim.tally.board_links);
            assert_eq!(
                sim.tally.refused + sim.tally.short + sim.tally.churn_links,
                0
            );
        }
    }

    // 3. A churning peer is dialled ONLY on a fallback verdict: its RPA
    //    is below every board address, so the strict sort can never
    //    elect it. With the fallback off, not one order dials it — and
    //    a peer that also never occupies a slot therefore leaves the
    //    whole strict row bit-identical to an empty room.
    let strict_alone = measure(
        10,
        FallbackSpec::Off,
        TargetChoice::MostFreeSlots,
        Churn::NONE,
        Mortality::IMMORTAL,
        Statics::NONE,
        Ledger::NONE,
        ConnectFailure::NONE,
    );
    let strict_churned = measure(
        10,
        FallbackSpec::Off,
        TargetChoice::MostFreeSlots,
        Churn::advertisers(1),
        Mortality::IMMORTAL,
        Statics::NONE,
        Ledger::NONE,
        ConnectFailure::NONE,
    );
    assert_eq!(
        (strict_churned.churn_links, strict_churned.refused),
        (0, 0),
        "the strict sort dialled a resolvable private address — the RPA class fact \
         `peer.rs` pins would have to be wrong"
    );
    assert_eq!(
        (
            strict_churned.disconnected,
            strict_churned.board_links,
            strict_churned.boardless
        ),
        (
            strict_alone.disconnected,
            strict_alone.board_links,
            strict_alone.boardless
        ),
        "an advertise-only churning peer changed a rule that can never dial it"
    );
    let eager_churned = measure(
        10,
        FallbackSpec::Eager,
        TargetChoice::MostFreeSlots,
        Churn::advertisers(1),
        Mortality::IMMORTAL,
        Statics::NONE,
        Ledger::NONE,
        ConnectFailure::NONE,
    );
    assert!(
        eager_churned.churn_links > 0,
        "the fallback never dialled the churning peer: there is no churn in the model"
    );

    // 4. The confound, pinned in the direction that flatters the churn
    //    rows: a churning peer that DIALS holds an incoming slot, which
    //    makes that board go dark one board-link earlier and spreads
    //    the dialling load — the same quantity #375 item 3 optimises.
    //    Under the strict rule, which can never dial the peer back,
    //    that is the ONLY effect left, and it makes the strict row look
    //    BETTER with a phone in the room. A churn row that improves is
    //    this, never a defence against churn.
    // (On the table's own strict row, `TargetChoice::FirstSeen`: the
    // spread only has room to help where the choice has not already
    // spread the load — under `MostFreeSlots` the phone's slot adds
    // saturation instead of relieving it, 520 against 498.)
    let strict_spread = measure(
        10,
        FallbackSpec::Off,
        TargetChoice::FirstSeen,
        Churn::NONE,
        Mortality::IMMORTAL,
        Statics::NONE,
        Ledger::NONE,
        ConnectFailure::NONE,
    );
    let strict_dialled = measure(
        10,
        FallbackSpec::Off,
        TargetChoice::FirstSeen,
        Churn::phones(1),
        Mortality::IMMORTAL,
        Statics::NONE,
        Ledger::NONE,
        ConnectFailure::NONE,
    );
    assert!(
        strict_dialled.saturated < strict_spread.saturated,
        "the dialling churn peer stopped spreading incoming load (saturated {} against {}); \
         the strict row's improvement had a different cause and the table needs re-reading",
        strict_dialled.saturated,
        strict_spread.saturated
    );
    assert!(
        strict_dialled.disconnected < strict_spread.disconnected,
        "the strict row no longer improves with a dialling churn peer ({} against {}); \
         re-measure the confound before publishing the table",
        strict_dialled.disconnected,
        strict_spread.disconnected
    );
}

/// #412 in the steady state: the same policies with board-to-board
/// links that END ([`Mortality`]), so a board that won a board link
/// dials again when it loses one. The formation-phase tables above are
/// the same instrument with `Mortality::IMMORTAL`.
///
/// Three rooms: the one the rig had (three boards, one phone, dialling),
/// and ten and twenty boards with 0, 1 and 2 churning peers. Two
/// policies, everything else held equal: the address order the rig ran
/// under (`eager/mostfree`, what shipped until 2026-09-25) and the
/// shipped one (`eager/rotatinglast`).
///
/// The numbers, per 1000 orders: `freed%` is the share of the dials that
/// spent a FREED outgoing slot which went to a churning peer — the rig's
/// own reading, `feld-t114` 7 of 7 — then board-to-board links formed,
/// dials per link that lasted, and split board graphs (the snapshot at
/// the horizon, one sample per order).
///
/// What this test holds on to is the instrument and the mechanism, in
/// the module docs' terms:
///
/// - **mortality fires and frees both slots**, or nothing below is a
///   steady state at all;
/// - **the address order hands the freed slot to the phone**, which is
///   what the rig measured at 100 % and what makes this harness an
///   answer to it rather than a second question;
/// - **the shipped order takes it back**, and the size of that is the
///   number the next order gets measured against.
#[test]
fn the_steady_state_freed_slot_goes_to_the_phone_under_the_address_order() {
    let mut rows = Vec::new();
    println!(
        "#412 steady state — per {ORDERS} orders; freed% = share of dials spending a FREED \
         outgoing"
    );
    println!(
        "slot that went to a churning peer (the rig read 7 of 7 = 100 %), bb = board-to-board \
         links"
    );
    println!(
        "formed, d/use = dials per link that lasted, disc = split board graphs at the horizon"
    );
    println!(
        "{:<4} {:<6} {:<19} {:>8} {:>7} {:>7} {:>6} {:>7} {:>6} {:>6} {:>7}",
        "n", "churn", "policy", "life", "freed%", "top%", "solo%", "bb", "d/use", "disc", "deaths"
    );
    for (n, churn) in [
        (3usize, Churn::phones(1)),
        (10, Churn::NONE),
        (10, Churn::phones(1)),
        (10, Churn::phones(2)),
        (20, Churn::NONE),
        (20, Churn::phones(1)),
        (20, Churn::phones(2)),
    ] {
        for (choice, label) in [
            (TargetChoice::MostFreeSlots, "eager/mostfree"),
            (TargetChoice::RotatingLast, "eager/rotatinglast"),
        ] {
            for (mortality, life) in [
                (Mortality::IMMORTAL, "immortal"),
                (Mortality::CAPTURE_SHORT_MODE, "45s"),
                (Mortality::CAPTURE_TAIL, "600s"),
            ] {
                let outcome = measure(
                    n,
                    FallbackSpec::Eager,
                    choice,
                    churn,
                    mortality,
                    Statics::NONE,
                    Ledger::NONE,
                    ConnectFailure::NONE,
                );
                println!(
                    "{n:<4} {:<6} {label:<19} {life:>8} {:>7} {:>7} {:>6} {:>7} {:>6} {:>6} \
                     {:>7}",
                    churn.peers,
                    Ratio(outcome.share_of_freed_slots_to_churn()),
                    Ratio(outcome.share_of_top_freed_slots_to_churn()),
                    Ratio(outcome.share_of_top_churn_dials_with_no_board_on_offer()),
                    outcome.board_links,
                    Ratio(outcome.dials_per_useful()),
                    outcome.disconnected,
                    outcome.deaths,
                );
                rows.push(((n, churn.peers, label, life), outcome));
            }
        }
    }
    let pick = |n: usize, peers: usize, label: &str, life: &str| -> &Outcome {
        &rows
            .iter()
            .find(|((rn, rp, rl, rlife), _)| {
                *rn == n && *rp == peers && *rl == label && *rlife == life
            })
            .expect("the row was measured above")
            .1
    };

    // 1. The instrument. Without deaths there is no steady state, and a
    //    death that freed only the central's slot would leave the
    //    peripheral's occupied forever — which the `bb` column would
    //    hide, because a board with three stale incoming links still
    //    forms outgoing ones.
    for &(n, peers) in &[(3usize, 1usize), (10, 0), (10, 1), (20, 0), (20, 2)] {
        for life in ["45s", "600s"] {
            let mortal = pick(n, peers, "eager/rotatinglast", life);
            assert!(
                mortal.deaths > 0,
                "n={n} churn={peers} life={life}: no board-to-board link ever died — \
                 the mortality parameter is inert and every row below is formation phase"
            );
            assert!(
                mortal.refill_dials > 0,
                "n={n} churn={peers} life={life}: no dial ever spent a freed slot"
            );
        }
        let immortal = pick(n, peers, "eager/rotatinglast", "immortal");
        assert_eq!(
            immortal.deaths, 0,
            "n={n} churn={peers}: a link died under Mortality::IMMORTAL"
        );
    }

    // 2. 303's tier, counted rather than assumed: no advertisement in
    //    this model carries PERIPHERAL_ONLY (boards advertise
    //    `LOCAL_CAPS = 0`, `columba.rs:213`, and the phone no record at
    //    all), so `DialPreference::CannotDialUs` never has a candidate
    //    and the tier's steady-state value is exactly zero — the same
    //    zero it has in the formation tables, for the same structural
    //    reason and not by seed. A switch would print two identical
    //    tables; this counts the offers the tier could have promoted.
    let mut offers = 0usize;
    for ((_, _, _, _), outcome) in &rows {
        offers += outcome.offers;
        assert_eq!(
            outcome.offers_cannot_dial, 0,
            "an offer reached the cannot-dial-us tier: some peer in the model now \
             advertises PERIPHERAL_ONLY, and the tier rows have to be measured for real"
        );
    }
    println!("tier census: {offers} window offers, 0 in DialPreference::CannotDialUs");
    assert!(
        offers > 1_000_000,
        "the tier census saw only {offers} offers, too few to say the tier never fired"
    );

    // 3. The rig's room and the rig's board: `feld-t114` is dialled by
    //    both the others, so it holds the highest address, so the v2.2
    //    rule leaves it no strict board candidate and every dial it
    //    makes is a fallback dial. Its freed slot went to the phone 7
    //    times out of 7, and the instrument has to reach that
    //    neighbourhood before its verdict on any fix means anything. The
    //    ROOM AVERAGE is not that number and must not be read as it: the
    //    two lower-addressed boards have a strict candidate and re-link
    //    to a board (the `freed%` column is 6 % where `top%` is 100 %).
    for life in ["immortal", "45s", "600s"] {
        let rig = pick(3, 1, "eager/mostfree", life);
        let top = rig
            .share_of_top_freed_slots_to_churn()
            .expect("the rig's room frees the top board's slot");
        assert!(
            top >= 90.0,
            "life={life}: the rig's room under the rig's policy gave the top board's freed \
             slot to the phone only {top:.0} % of the time ({} of {}); the rig measured 7 of \
             7, so the model has lost the mechanism it is supposed to reproduce",
            rig.refill_dials_top_churn,
            rig.refill_dials_top
        );
    }

    // 4. And the finding this instrument exists to produce: in the rig's
    //    own room the SHIPPED fallback order does not defend that slot
    //    either — it is 100 % under both orders. The reason is measured
    //    twice over, here and in
    //    `a_board_that_has_spent_an_incoming_slot_loses_the_fallback_to_a_silent_phone`:
    //    it is not that no board was on offer (`solo%` is a few per cent
    //    at most), it is that a board holding an incoming link deficits
    //    by one on the term ABOVE the rotating-last order, while a phone
    //    that advertises no record at all deficits by zero. In a
    //    three-board room every board is carrying an incoming link, so
    //    the phone wins the second term before the fourth is consulted.
    for life in ["immortal", "45s", "600s"] {
        let shipped = pick(3, 1, "eager/rotatinglast", life);
        let top = shipped
            .share_of_top_freed_slots_to_churn()
            .expect("the rig's room frees the top board's slot");
        assert!(
            top >= 90.0,
            "life={life}: the shipped fallback order now DEFENDS the top board's freed slot \
             in the rig's three-board room ({top:.0} %). That is the outcome the next order \
             is for; if it arrived from somewhere else, the reasoning in the module docs \
             under \"Steady state, link mortality\" is stale and has to be rewritten"
        );
    }
    // The reason, on the row where the claim is made: at the capture's
    // short mode a board WAS on offer in 97 % of the top board's churn
    // dials and lost the window. The other two rows carry a second,
    // independent reason as well — 57 % of those dials have no board on
    // offer at all when links never end, and 38 % at the 600 s mean —
    // because a room of three boards whose links stand has every board
    // excluded by §4.5 already. Neither reason is an order's to fix, and
    // they are not summed here: this pins the one the mechanism cell
    // reproduces.
    let solo = pick(3, 1, "eager/rotatinglast", "45s")
        .share_of_top_churn_dials_with_no_board_on_offer()
        .expect("those dials happened");
    assert!(
        solo < 10.0,
        "{solo:.0} % of the top board's churn dials at the 45 s mean had no board on offer \
         at all, so the 100 % above is a room too small to hold a choice and NOT the key's \
         term order — re-read the finding before acting on it"
    );

    // 5. The order is not worthless, and the boundary is where a board
    //    with every slot still free exists to be preferred: at twenty
    //    boards the shipped order takes the top board's share from
    //    ~100 % down (measured 51 % at the 45 s row and 7 % at 600 s).
    for life in ["45s", "600s"] {
        let address = pick(20, 1, "eager/mostfree", life)
            .share_of_top_freed_slots_to_churn()
            .expect("measured above");
        let shipped = pick(20, 1, "eager/rotatinglast", life)
            .share_of_top_freed_slots_to_churn()
            .expect("measured above");
        assert!(
            shipped * 1.5 < address,
            "n=20 life={life}: the shipped order no longer cuts the top board's freed-slot \
             share in a room that holds a board with slots to spare (shipped {shipped:.0} %, \
             address order {address:.0} %)"
        );
    }

    // 6. And the direction nobody should quote the wrong way round: for
    //    the room as a WHOLE, mortality LOWERS the share of dials spent
    //    on the phone rather than raising it, because a death hands both
    //    boards a strict candidate back and they re-link in the next
    //    round. The rig's steady-state number is worse than the
    //    formation table's not because sessions end but because of the
    //    board it was read on.
    for n in [10usize, 20] {
        let formation = pick(n, 1, "eager/mostfree", "immortal");
        let steady = pick(n, 1, "eager/mostfree", "45s");
        println!(
            "n={n} one phone, address order: dials spent on churn {:.1} % formation -> \
             {:.1} % steady; freed slots {} -> {}",
            formation.share_spent_on_churn(),
            steady.share_spent_on_churn(),
            Ratio(formation.share_of_freed_slots_to_churn()),
            Ratio(steady.share_of_freed_slots_to_churn()),
        );
        assert!(
            steady.share_spent_on_churn() < formation.share_spent_on_churn(),
            "n={n}: link mortality raised the share of the room's dials the address order \
             spends on the phone ({:.0} % steady against {:.0} % formation). The module \
             docs say it falls and say why; one of the two is now wrong",
            steady.share_spent_on_churn(),
            formation.share_spent_on_churn()
        );
    }
}

/// The four policies #412 steady state 2 compares: the address order the
/// shipped one replaced, the shipped one, and item 3's two candidates.
const POLICIES: [(TargetChoice, &str); 4] = [
    (TargetChoice::MostFreeSlots, "eager/mostfree"),
    (TargetChoice::RotatingLast, "eager/rotatinglast"),
    (TargetChoice::GroupAboveDeficit, "eager/groupfirst"),
    (TargetChoice::SilenceIsFull, "eager/silencefull"),
];

/// The three mortality rows, as the steady-state table states them.
const LIFETIMES: [(Mortality, &str); 3] = [
    (Mortality::IMMORTAL, "immortal"),
    (Mortality::CAPTURE_SHORT_MODE, "45s"),
    (Mortality::CAPTURE_TAIL, "600s"),
];
/// The rig's room with the fourth kind in it (#412 steady state 2):
/// three boards, one phone, and the static peer [`Statics`] names — the
/// solar node. The columns are 306's plus `stat%`, the share of the TOP
/// board's freed-slot dials that went to the static peer. That is the
/// column the rig's own ledger reads in: of `feld-t114`'s 65 outgoing
/// links, 42 went to the solar node, 20 to the phone and 3 to a board,
/// and every one of the 25 it made after 2026-09-25 — when the shipped
/// fallback order reached the boards — went to the solar node.
///
/// Four policies, because item 3 of the batch asks the same table of the
/// two candidate keys as well: the address order the shipped one
/// replaced, the shipped one, and the two ways of taking the free ride
/// away ([`TargetChoice::GroupAboveDeficit`] and
/// [`TargetChoice::SilenceIsFull`]).
///
/// Three rooms, because the fourth kind's ADDRESS is the other half of
/// the mechanism: `Statics::NONE` is 306's room and every cell of it is
/// pinned here as an equality; [`Statics::rig`] puts the peer below every
/// board as `d916e2923ed2` was; [`Statics::drawn`] draws its address like
/// a board's, which is a different room and not a worse seed — see the
/// assertion at the end.
#[test]
fn the_static_peer_takes_the_freed_slot_the_phone_was_blamed_for() {
    let mut rows = Vec::new();
    println!(
        "#412 steady state 2 — the rig's room (3 boards, 1 phone), per {ORDERS} orders. \
         freed% = share"
    );
    println!(
        "of dials spending a FREED outgoing slot that went to the PHONE; stat% and top% = the \
         top board's"
    );
    println!(
        "freed slot to the static peer and to the phone; sb = links formed to the static peer"
    );
    println!(
        "{:<8} {:<19} {:>8} {:>7} {:>7} {:>7} {:>6} {:>7} {:>7} {:>6} {:>6}",
        "statics",
        "policy",
        "life",
        "freed%",
        "stat%",
        "top%",
        "solo%",
        "sb",
        "bb",
        "d/use",
        "disc"
    );
    for (statics, room) in [
        (Statics::NONE, "none"),
        (Statics::rig(1), "rig"),
        (Statics::drawn(1), "drawn"),
    ] {
        for (choice, label) in POLICIES {
            for (mortality, life) in LIFETIMES {
                let outcome = measure(
                    3,
                    FallbackSpec::Eager,
                    choice,
                    Churn::phones(1),
                    mortality,
                    statics,
                    Ledger::NONE,
                    ConnectFailure::NONE,
                );
                println!(
                    "{room:<8} {label:<19} {life:>8} {:>7} {:>7} {:>7} {:>6} {:>7} {:>7} \
                     {:>6} {:>6}",
                    Ratio(outcome.share_of_freed_slots_to_churn()),
                    Ratio(outcome.share_of_top_freed_slots_to_static()),
                    Ratio(outcome.share_of_top_freed_slots_to_churn()),
                    Ratio(outcome.share_of_top_churn_dials_with_no_board_on_offer()),
                    outcome.static_links,
                    outcome.board_links,
                    Ratio(outcome.dials_per_useful()),
                    outcome.disconnected,
                );
                rows.push(((room, label, life), outcome));
            }
        }
    }
    let pick = |room: &str, label: &str, life: &str| -> &Outcome {
        &rows
            .iter()
            .find(|((rr, rl, rlife), _)| *rr == room && *rl == label && *rlife == life)
            .expect("the row was measured above")
            .1
    };

    // 1. The parameter is a parameter: with no static peer in it, the
    //    room is 306's room to the link, under both orders it published.
    //    Equalities, not bounds — the static peer draws from a stream of
    //    its own and `Statics::NONE` takes nothing out of it.
    for policy in ["eager/mostfree", "eager/rotatinglast"] {
        for (life, bb) in [("immortal", 2040), ("45s", 26790), ("600s", 3904)] {
            let row = pick("none", policy, life);
            assert_eq!(
                (row.board_links, row.disconnected),
                (bb, 0),
                "{policy} {life}: the static peer moved a row measured without one"
            );
            let top = row
                .share_of_top_freed_slots_to_churn()
                .expect("the rig's room frees the top board's slot");
            assert!(
                top >= 99.0,
                "{policy} {life}: 306's finding (the top board's freed slot goes to the phone \
                 in the rig's three-board room) is {top:.2} % here, so the row it is read off \
                 has moved"
            );
            assert_eq!(
                row.static_links, 0,
                "a link to a static peer formed in a room that has none"
            );
        }
    }

    // 2. And the finding: the fourth kind takes that slot. Under the
    //    SHIPPED order at the capture's short mode the top board's freed
    //    slot goes to the static peer, not the phone — which is the rig's
    //    own ledger after 2026-09-25 (25 of 25 to the solar node) and
    //    exactly the column 306 had no kind for.
    let shipped = pick("rig", "eager/rotatinglast", "45s");
    let to_static = shipped
        .share_of_top_freed_slots_to_static()
        .expect("the room frees the top board's slot");
    let to_phone = shipped
        .share_of_top_freed_slots_to_churn()
        .expect("same denominator");
    assert!(
        to_static > 95.0 && to_phone < 5.0,
        "the shipped order sent the top board's freed slot to the static peer {to_static:.2} % \
         and to the phone {to_phone:.2} % of the time; the rig's post-2026-09-25 ledger is 25 \
         of 25 to the solar node, so the model no longer reproduces it"
    );

    // 3. The same room under the order the shipped one REPLACED gives it
    //    to the phone instead — which is the rig's ledger BEFORE that
    //    date (20 of `feld-t114`'s 65 links). The model reproduces both
    //    eras of the capture, and the only thing that moved between them
    //    is the fallback order.
    let address_order = pick("rig", "eager/mostfree", "45s")
        .share_of_top_freed_slots_to_churn()
        .expect("same denominator");
    assert!(
        address_order > 95.0,
        "under the address order the top board's freed slot went to the phone only \
         {address_order:.2} % of the time with the static peer present; the capture's earlier \
         era (the phone taking 20 of 65) is then unexplained"
    );

    // 4. The two candidate keys are indistinguishable in this model, in
    //    every column of every row — and that is a statement about the
    //    ROOM, not about the policies: the only peer here with no
    //    capability record is the phone, and the only peer with a
    //    rotating address is the phone, so demoting silence and demoting
    //    the rotating group demote the same single peer. The unit cell
    //    `the_two_candidate_keys_are_two_different_policies` separates
    //    them with a peer in one set and not the other, which is the
    //    advertisement no Columba sends today.
    for room in ["none", "rig", "drawn"] {
        for (_, life) in LIFETIMES {
            let group = pick(room, "eager/groupfirst", life);
            let silence = pick(room, "eager/silencefull", life);
            assert_eq!(
                (
                    group.board_links,
                    group.static_links,
                    group.disconnected,
                    group.refill_dials_top_churn,
                    group.refill_dials_top_static
                ),
                (
                    silence.board_links,
                    silence.static_links,
                    silence.disconnected,
                    silence.refill_dials_top_churn,
                    silence.refill_dials_top_static
                ),
                "{room} {life}: the two candidate keys now differ in the Monte Carlo, so a \
                 peer that is in one of the two sets and not the other has appeared in the \
                 model and the tables have to be read as two policies"
            );
        }
    }

    // 5. Where the fourth kind's address sits is the other half of it. At
    //    the rig's own order — below every board — no board ever STRICTLY
    //    elects it, so it takes fallback dials only and the board graph
    //    survives. Drawn like a board's, every board below it dials it
    //    strictly and spends its one outgoing slot there, and the
    //    three-board graph splits in most orders. The rig's room was the
    //    benign draw of the two.
    let rig_split = pick("rig", "eager/rotatinglast", "immortal").disconnected;
    let drawn_split = pick("drawn", "eager/rotatinglast", "immortal").disconnected;
    assert!(
        drawn_split > 10 * rig_split.max(1),
        "a static peer drawn at a board's address split the board graph {drawn_split} times \
         per {ORDERS} orders against {rig_split} at the rig's address order; if those are now \
         the same, the strict-dial half of the mechanism is gone"
    );
}

/// The same tables at ten and twenty boards (#412 steady state 2, item
/// 2), one phone and one static peer, both lifetimes — the sizes 306
/// measured, with the kind it was missing.
///
/// The immortal row is left out here on purpose: with links that stand, a
/// room of ten or twenty boards has every board linked long before the
/// horizon and the freed-slot column has almost no denominator. It is in
/// the rig's three-board table above, where it does.
#[test]
fn the_two_candidate_keys_with_a_static_peer_at_ten_and_twenty_boards() {
    let mut rows = Vec::new();
    println!("#412 steady state 2 — n=10/20, one phone + one static peer, per {ORDERS} orders");
    println!(
        "{:<4} {:<19} {:>8} {:>7} {:>7} {:>7} {:>7} {:>7} {:>8} {:>6} {:>6}",
        "n", "policy", "life", "freed%", "sfreed%", "stat%", "top%", "sb", "bb", "d/use", "disc"
    );
    for n in [10usize, 20] {
        for (choice, label) in POLICIES {
            for (mortality, life) in LIFETIMES {
                if mortality == Mortality::IMMORTAL {
                    continue;
                }
                let outcome = measure(
                    n,
                    FallbackSpec::Eager,
                    choice,
                    Churn::phones(1),
                    mortality,
                    Statics::rig(1),
                    Ledger::NONE,
                    ConnectFailure::NONE,
                );
                println!(
                    "{n:<4} {label:<19} {life:>8} {:>7} {:>7} {:>7} {:>7} {:>7} {:>8} {:>6} \
                     {:>6}",
                    Ratio(outcome.share_of_freed_slots_to_churn()),
                    Ratio(outcome.share_of_freed_slots_to_static()),
                    Ratio(outcome.share_of_top_freed_slots_to_static()),
                    Ratio(outcome.share_of_top_freed_slots_to_churn()),
                    outcome.static_links,
                    outcome.board_links,
                    Ratio(outcome.dials_per_useful()),
                    outcome.disconnected,
                );
                rows.push(((n, label, life), outcome));
            }
        }
    }
    let pick = |n: usize, label: &str, life: &str| -> &Outcome {
        &rows
            .iter()
            .find(|((rn, rl, rlife), _)| *rn == n && *rl == label && *rlife == life)
            .expect("the row was measured above")
            .1
    };
    // The finding of the three-board table is not a small-room artefact:
    // at ten and twenty boards the shipped order spends the top board's
    // freed slot on the static peer too, and the phone's share of it is
    // in the noise. 306's open question — "the shipped order cannot
    // defend the freed slot in a small room" — was a room with a member
    // missing, at every size.
    for n in [10usize, 20] {
        let shipped = pick(n, "eager/rotatinglast", "45s");
        let to_static = shipped
            .share_of_top_freed_slots_to_static()
            .expect("the room frees the top board's slot");
        let to_phone = shipped
            .share_of_top_freed_slots_to_churn()
            .expect("same denominator");
        assert!(
            to_static > 95.0 && to_phone < 5.0,
            "n={n}: with a static peer in the room the shipped order gave the top board's \
             freed slot to the static peer {to_static:.2} % and to the phone {to_phone:.2} %"
        );
        // And the address order still gives it to the phone at both
        // sizes, so the static peer is not simply swallowing every dial:
        // it is the ORDER that elects it.
        let address = pick(n, "eager/mostfree", "45s")
            .share_of_top_freed_slots_to_churn()
            .expect("same denominator");
        assert!(
            address > 95.0,
            "n={n}: the address order no longer sends the top board's freed slot to the phone \
             ({address:.2} %), so the two orders can no longer be compared on this column"
        );
    }
    // What the candidate keys add here: at the 600 s row, where links
    // stand and the phone is sometimes the only thing left, they take the
    // residual to zero (measured 8.87 % -> 0 % at n=10, 0.28 % -> 0 % at
    // n=20) and they cost nothing on the `bb` column.
    for n in [10usize, 20] {
        let shipped = pick(n, "eager/rotatinglast", "600s")
            .share_of_top_freed_slots_to_churn()
            .expect("measured above");
        let candidate = pick(n, "eager/groupfirst", "600s")
            .share_of_top_freed_slots_to_churn()
            .expect("measured above");
        assert!(
            candidate <= shipped,
            "n=600s n={n}: the candidate key spends MORE of the top board's freed slots on the \
             phone than the shipped order ({candidate:.2} % against {shipped:.2} %)"
        );
    }
}

/// What the two candidate keys cost where #375's guarantee lives: an
/// empty room of boards, and a room with a phone but no static peer —
/// which is the room 306's finding was made in.
///
/// Both questions are here because they are the price side of item 4's
/// sentence, and the answer is measured rather than argued: in the empty
/// room the candidates are the shipped order to the link, and in the
/// three-board room with a phone they are what defends the slot.
#[test]
fn the_two_candidate_keys_cost_the_empty_room_nothing() {
    for n in [10usize, 20] {
        let mut empty = Vec::new();
        for (choice, label) in POLICIES {
            let outcome = measure(
                n,
                FallbackSpec::Eager,
                choice,
                Churn::NONE,
                Mortality::IMMORTAL,
                Statics::NONE,
                Ledger::NONE,
                ConnectFailure::NONE,
            );
            println!(
                "n={n} {label} empty room: disc {} boardless {} bb {} d/use {}",
                outcome.disconnected,
                outcome.boardless,
                outcome.board_links,
                Ratio(outcome.dials_per_useful())
            );
            empty.push((label, outcome));
        }
        // #375's own guarantee: 0 split graphs and 0 boardless boards at
        // both sizes, and `bb` and `d/use` identical to the shipped
        // order's. A candidate that bought the freed slot by giving this
        // back would be the deviation #375 does not allow; neither is.
        let (_, shipped) = empty
            .iter()
            .find(|(label, _)| *label == "eager/rotatinglast")
            .expect("the shipped policy is in the table");
        for (label, outcome) in &empty {
            assert_eq!(
                (
                    outcome.disconnected,
                    outcome.boardless,
                    outcome.board_links,
                    outcome.dials
                ),
                (0, 0, shipped.board_links, shipped.dials),
                "n={n} {label}: the candidate key changed the empty room, which is where \
                 #375's guarantee is"
            );
        }
    }
    // The room 306's finding was made in: three boards and a phone, no
    // static peer. Here the candidates DO defend the freed slot — the
    // top board's share of it spent on the phone falls from 100 % to
    // under 1 % at the capture's short mode — and they hand back more
    // board-to-board links than the shipped order, not fewer.
    for (choice, label) in POLICIES {
        let outcome = measure(
            3,
            FallbackSpec::Eager,
            choice,
            Churn::phones(1),
            Mortality::CAPTURE_SHORT_MODE,
            Statics::NONE,
            Ledger::NONE,
            ConnectFailure::NONE,
        );
        println!(
            "n=3 {label} one phone, 45 s, no static peer: top% {} bb {} disc {} d/use {}",
            Ratio(outcome.share_of_top_freed_slots_to_churn()),
            outcome.board_links,
            outcome.disconnected,
            Ratio(outcome.dials_per_useful())
        );
        let top = outcome
            .share_of_top_freed_slots_to_churn()
            .expect("the room frees the top board's slot");
        let defended = matches!(
            choice,
            TargetChoice::GroupAboveDeficit | TargetChoice::SilenceIsFull
        );
        assert_eq!(
            top < 1.0,
            defended,
            "{label}: the top board's freed slot went to the phone {top:.2} % of the time in \
             the room 306 measured; the candidate keys are the ones that defend it and the \
             two shipped orders are the ones that do not"
        );
        assert_eq!(
            outcome.disconnected, 0,
            "{label}: a split board graph in a three-board room with one phone"
        );
    }
}

/// The ledger configurations the tables below sweep (#412 part 2). The
/// defaults are `k` in {2, 3, 5} at T = [`USEFUL_SESSION_MS`] and the
/// [`Ledger::PAUSE_ROUNDS`] pause; the last four move one parameter at a
/// time off the middle of that sweep, so every row differs from `k=3` in
/// exactly one number.
const LEDGERS: [(Ledger, &str); 8] = [
    (Ledger::NONE, "off"),
    (Ledger::after(2), "k=2"),
    (Ledger::after(3), "k=3"),
    (Ledger::after(5), "k=5"),
    (Ledger::with_t(3, LINK_ABANDONED_MS), "k=3 T=30s"),
    (Ledger::with_t(3, LINK_TIMEOUT_MS), "k=3 T=45s"),
    (
        Ledger::with_pause(3, FALLBACK_AFTER_ROUNDS),
        "k=3 pause=30s",
    ),
    (
        Ledger::with_pause(3, 6 * FALLBACK_AFTER_ROUNDS),
        "k=3 pause=3m",
    ),
];

/// #412 part 2 in the room the rig has: three boards, one phone, with and
/// without the solar node, under the SHIPPED candidate order and every
/// [`Ledger`] the sweep above states.
///
/// The columns are 311's plus the two the ledger adds: `hold` counts the
/// windows the pause held shut where the board would otherwise have
/// spent a fallback dial, and `hold%` is the share of those windows that
/// had no board on offer at all — the solo window, the one #412 part 2
/// exists for. `bb` and `disc` are where the cost of holding the OTHER
/// windows shut shows up.
#[test]
fn the_dial_ledger_and_the_solo_window_in_the_rig_room() {
    let mut rows = Vec::new();
    println!(
        "#412 part 2 — the rig's room (3 boards, 1 phone), shipped order, per {ORDERS} orders. \
         `hold` is the windows the pause held shut, `hold%` the share of those with no board \
         on offer at all, `refused` the dials spent to be told the identity was already live."
    );
    println!(
        "{:<6} {:<14} {:>8} {:>7} {:>7} {:>6} {:>6} {:>7} {:>6} {:>7} {:>8} {:>6} {:>7} \
         {:>6} {:>5}",
        "room",
        "ledger",
        "life",
        "freed%",
        "top%",
        "solo%",
        "top#",
        "hold",
        "hold%",
        "dials",
        "refused",
        "sb",
        "bb",
        "d/use",
        "disc"
    );
    for (statics, room) in [(Statics::NONE, "none"), (Statics::rig(1), "rig")] {
        for (ledger, label) in LEDGERS {
            for (mortality, life) in LIFETIMES {
                let outcome = measure(
                    3,
                    FallbackSpec::Eager,
                    TargetChoice::RotatingLast,
                    Churn::phones(1),
                    mortality,
                    statics,
                    ledger,
                    ConnectFailure::NONE,
                );
                println!(
                    "{room:<6} {label:<14} {life:>8} {:>7} {:>7} {:>6} {:>6} {:>7} {:>6} \
                     {:>7} {:>8} {:>6} {:>7} {:>6} {:>5}",
                    Ratio(outcome.share_of_freed_slots_to_churn()),
                    Ratio(outcome.share_of_top_freed_slots_to_churn()),
                    Ratio(outcome.share_of_top_churn_dials_with_no_board_on_offer()),
                    outcome.refill_dials_top_churn,
                    outcome.held_windows,
                    Ratio(outcome.share_of_held_windows_solo()),
                    outcome.dials,
                    outcome.refused,
                    outcome.static_links,
                    outcome.board_links,
                    Ratio(outcome.dials_per_useful()),
                    outcome.disconnected,
                );
                rows.push(((room, label, life), outcome));
            }
        }
    }
    let pick = |room: &str, label: &str, life: &str| -> &Outcome {
        &rows
            .iter()
            .find(|((rr, rl, rlife), _)| *rr == room && *rl == label && *rlife == life)
            .expect("the row was measured above")
            .1
    };
    // The parameter is a parameter: with no ledger the room is 311's room
    // to the link, in both of its rooms and at every lifetime. Equalities
    // — the ledger draws from no stream at all, so `Ledger::NONE` cannot
    // move a seed.
    for (room, life, bb, disc) in [
        ("none", "immortal", 2040, 0),
        ("none", "45s", 26_790, 0),
        ("none", "600s", 3904, 0),
        ("rig", "immortal", 2090, 0),
        ("rig", "45s", 26_840, 0),
        ("rig", "600s", 3976, 22),
    ] {
        let off = pick(room, "off", life);
        assert_eq!(
            (off.board_links, off.disconnected, off.held_windows),
            (bb, disc, 0),
            "{room} {life}: the ledger moved a row measured without one"
        );
    }

    // 1. The pause the order proposed — one scan cycle — holds nothing
    //    shut, in every room and at every lifetime, and the reason is
    //    structural rather than a seed: a teardown resets the strict
    //    clock, so the board spends one full scan cycle in strict mode
    //    before it can reach a fallback dial at all, and a pause of
    //    exactly that length expires in the round the fallback becomes
    //    available. It is in the sweep as the control of the pause knob:
    //    a ledger whose pause is too short is a ledger that does nothing,
    //    which is not the same statement as "the ledger does nothing".
    for (_, life) in LIFETIMES {
        for room in ["none", "rig"] {
            let one_cycle = pick(room, "k=3 pause=30s", life);
            let off = pick(room, "off", life);
            assert_eq!(
                (
                    one_cycle.held_windows,
                    one_cycle.board_links,
                    one_cycle.dials
                ),
                (0, off.board_links, off.dials),
                "{room} {life}: a pause of one scan cycle now withholds a dial, so the strict \
                 phase after a teardown is no longer one scan cycle long and \
                 `Ledger::PAUSE_ROUNDS` has to be re-derived"
            );
        }
    }

    // 2. The room with no fourth kind in it is the room 311 left open:
    //    `solo%` is 57 % of the top board's churn dials when links stand
    //    and 38 % at the 600 s mean, and no candidate order can defend a
    //    window with one candidate in it. The ledger does: it holds those
    //    windows shut, and it costs this room NOTHING — not one
    //    board-to-board link, not one split graph, at every K in the
    //    sweep. Every board link here is won on a strict verdict, which
    //    the ledger never touches.
    for k in ["k=2", "k=3", "k=5"] {
        for (_, life) in LIFETIMES {
            let with = pick("none", k, life);
            let off = pick("none", "off", life);
            assert_eq!(
                (with.board_links, with.disconnected),
                (off.board_links, off.disconnected),
                "none {k} {life}: the ledger cost the phone-only room board links or \
                 connectivity"
            );
            assert!(
                with.held_windows > 0,
                "none {k} {life}: the ledger never held a window shut, so the row measures \
                 nothing"
            );
            assert!(
                with.dials < off.dials,
                "none {k} {life}: the ledger held {} windows shut and the room still made {} \
                 dials against {} without it",
                with.held_windows,
                with.dials,
                off.dials
            );
        }
    }
    // And what it bought, on the two rows whose windows are the solo
    // ones: dials per link that lasted, which is #412's third number.
    for (life, ceiling) in [("immortal", 1.70), ("600s", 1.55)] {
        let with = pick("none", "k=3", life);
        let off = pick("none", "off", life);
        let (with_ratio, off_ratio) = (
            with.dials_per_useful().expect("dials were made"),
            off.dials_per_useful().expect("dials were made"),
        );
        assert!(
            with_ratio < ceiling && with_ratio < off_ratio,
            "none k=3 {life}: dials per useful link is {with_ratio:.2} with the ledger and \
             {off_ratio:.2} without it"
        );
    }

    // 3. And in the room the rig actually has, the one with the solar
    //    node, the ledger is nearly inert — which is the right answer,
    //    not a disappointment: the shipped order already sends the top
    //    board's freed slot to a peer whose sessions last (0.32 % to the
    //    phone), so there is no run of waste to remember. The bound is
    //    what matters: it must not take the room's links away.
    for k in ["k=2", "k=3", "k=5"] {
        for (_, life) in LIFETIMES {
            let with = pick("rig", k, life);
            let off = pick("rig", "off", life);
            let lost = off.board_links.saturating_sub(with.board_links);
            assert!(
                lost * 100 <= off.board_links,
                "rig {k} {life}: the ledger cost {lost} of {} board-to-board links, past the \
                 1 % the sweep measures it at",
                off.board_links
            );
            assert!(
                with.disconnected <= off.disconnected,
                "rig {k} {life}: split board graphs went from {} to {} with the ledger — it \
                 has to cost this room no connectivity at all, and at K = 2 it measurably \
                 improves it",
                off.disconnected,
                with.disconnected
            );
        }
    }
}

/// One size's ledger table (#412 part 2, item 2): the three rooms 311
/// measured — boards alone, boards with a phone, and the rig's room with
/// the solar node in it as well — under the SHIPPED candidate order, at
/// the capture's two lifetimes, for each [`Ledger`] handed in.
///
/// The immortal row is measured for the room of boards alone only: that
/// is #375's own guarantee cell (`disc` = 0 with every board linked), and
/// at these sizes a room with a churning peer in it has almost no
/// freed-slot denominator before the horizon — the rig's three-board
/// table above is where the immortal churn rows live.
///
/// `refused` is the column the ledger is really about at these sizes: a
/// dial refused post-connect is a connect, a discovery and an identity
/// read spent to be told the identity was already live.
fn ledger_rows(
    n: usize,
    ledgers: &[(Ledger, &'static str)],
) -> Vec<((&'static str, &'static str, &'static str), Outcome)> {
    println!("#412 part 2 — n={n}, shipped order, per {ORDERS} orders");
    println!(
        "{:<6} {:<6} {:>8} {:>7} {:>7} {:>7} {:>6} {:>8} {:>8} {:>6} {:>5} {:>6}",
        "room",
        "ledger",
        "life",
        "freed%",
        "top%",
        "hold",
        "hold%",
        "bb",
        "refused",
        "d/use",
        "disc",
        "board-"
    );
    let mut rows = Vec::new();
    for (churn, statics, room) in [
        (Churn::NONE, Statics::NONE, "empty"),
        (Churn::phones(1), Statics::NONE, "phone"),
        (Churn::phones(1), Statics::rig(1), "rig"),
    ] {
        for &(ledger, label) in ledgers {
            for (mortality, life) in LIFETIMES {
                if mortality == Mortality::IMMORTAL && room != "empty" {
                    continue;
                }
                let outcome = measure(
                    n,
                    FallbackSpec::Eager,
                    TargetChoice::RotatingLast,
                    churn,
                    mortality,
                    statics,
                    ledger,
                    ConnectFailure::NONE,
                );
                println!(
                    "{room:<6} {label:<6} {life:>8} {:>7} {:>7} {:>7} {:>6} {:>8} {:>8} {:>6} \
                     {:>5} {:>6}",
                    Ratio(outcome.share_of_freed_slots_to_churn()),
                    Ratio(outcome.share_of_top_freed_slots_to_churn()),
                    outcome.held_windows,
                    Ratio(outcome.share_of_held_windows_solo()),
                    outcome.board_links,
                    outcome.refused,
                    Ratio(outcome.dials_per_useful()),
                    outcome.disconnected,
                    outcome.boardless,
                );
                rows.push(((room, label, life), outcome));
            }
        }
    }
    rows
}

/// What the rows above have to hold whatever the size: the ledger's zero
/// is a zero, a room of boards whose links never end never records a
/// single outcome, and nothing the ledger does may cost #375's guarantee.
fn assert_ledger_costs_the_room_nothing(
    n: usize,
    rows: &[((&'static str, &'static str, &'static str), Outcome)],
    ledgers: &[(Ledger, &'static str)],
) {
    let pick = |room: &str, label: &str, life: &str| -> &Outcome {
        &rows
            .iter()
            .find(|((rr, rl, rlife), _)| *rr == room && *rl == label && *rlife == life)
            .expect("the row was measured above")
            .1
    };
    for &(ledger, label) in ledgers {
        if !ledger.keeps() {
            continue;
        }
        // 1. A room of boards whose links never end: no dial ever has a
        //    wasted outcome, so the ledger writes nothing and the row is
        //    the shipped row dial for dial. An equality, and the strongest
        //    statement available — it says the mechanism cannot fire
        //    there, not merely that the totals matched.
        let (with, off) = (
            pick("empty", label, "immortal"),
            pick("empty", "off", "immortal"),
        );
        assert_eq!(
            (
                with.held_windows,
                with.board_links,
                with.dials,
                with.disconnected,
                with.boardless
            ),
            (0, off.board_links, off.dials, 0, 0),
            "n={n} {label}: the ledger moved #375's own guarantee cell — a room of boards \
             with links that never end, where no dial can have a wasted outcome at all"
        );
        // 2. And with mortality on, where links DO die young and the
        //    ledger does fire in a room with no phone in it, it must not
        //    strand a board or split the graph. `bb` may move — a held
        //    window is a dial not made — and the bound is what the
        //    tables above measure it at.
        for (room, life) in [
            ("empty", "45s"),
            ("empty", "600s"),
            ("phone", "45s"),
            ("phone", "600s"),
            ("rig", "45s"),
            ("rig", "600s"),
        ] {
            let (with, off) = (pick(room, label, life), pick(room, "off", life));
            assert!(
                with.boardless <= off.boardless,
                "n={n} {room} {life} {label}: the ledger left {} boards with no board link at \
                 all against {} without it",
                with.boardless,
                off.boardless
            );
            let lost = off.board_links.saturating_sub(with.board_links);
            assert!(
                lost * 100 <= off.board_links,
                "n={n} {room} {life} {label}: the ledger cost {lost} of {} board-to-board \
                 links, past the 1 % the tables measure it at",
                off.board_links
            );
        }
    }
}

/// #412 part 2 at ten boards, with the K sweep the order asks for.
#[test]
fn the_dial_ledger_at_ten_boards() {
    let sweep = [
        (Ledger::NONE, "off"),
        (Ledger::after(2), "k=2"),
        (Ledger::after(3), "k=3"),
        (Ledger::after(5), "k=5"),
    ];
    let rows = ledger_rows(10, &sweep);
    let pick = |room: &str, label: &str, life: &str| -> &Outcome {
        &rows
            .iter()
            .find(|((rr, rl, rlife), _)| *rr == room && *rl == label && *rlife == life)
            .expect("the row was measured above")
            .1
    };
    // The parameter is a parameter, on the cells 311 published for this
    // size under the shipped order.
    for (room, life, bb, disc) in [
        ("empty", "immortal", 10_000, 0),
        ("empty", "45s", 122_812, 0),
        ("empty", "600s", 19_270, 12),
        ("phone", "45s", 118_040, 0),
        ("phone", "600s", 18_804, 14),
        ("rig", "45s", 116_710, 0),
        ("rig", "600s", 17_610, 32),
    ] {
        let off = pick(room, "off", life);
        assert_eq!(
            (off.board_links, off.disconnected, off.held_windows),
            (bb, disc, 0),
            "n=10 {room} {life}: the ledger moved a row 311 measured without one"
        );
    }
    assert_ledger_costs_the_room_nothing(10, &rows, &sweep);
    // The ledger is nearly INERT at this size, and that is a finding
    // rather than a gap: a board in a room of ten has strict candidates,
    // so it rarely reaches a fallback dial at all, and the whole thousand
    // orders hold four refused dials against the three-board room's
    // thousands. It still fires — the size does not switch the mechanism
    // off — and what it costs is nothing: split graphs and stranded
    // boards identical to the shipped row in every room, at every
    // lifetime, for every K.
    assert!(
        ["k=2", "k=3", "k=5"]
            .iter()
            .any(|k| pick("phone", k, "45s").held_windows > 0),
        "n=10: the ledger never held one window shut in the phone room, so the rows above \
         measure nothing at this size"
    );
    for k in ["k=2", "k=3", "k=5"] {
        for (room, life) in [
            ("empty", "45s"),
            ("empty", "600s"),
            ("phone", "45s"),
            ("phone", "600s"),
            ("rig", "45s"),
            ("rig", "600s"),
        ] {
            let (with, off) = (pick(room, k, life), pick(room, "off", life));
            assert_eq!(
                (with.disconnected, with.boardless),
                (off.disconnected, off.boardless),
                "n=10 {room} {life} {k}: the ledger moved connectivity at a size where it \
                 holds at most {} of the room's windows shut",
                with.held_windows
            );
        }
    }
}

/// #412 part 2 at twenty boards, the size #375's guarantee is stated at.
/// Two ledgers rather than the sweep: the K sweep is in the two tables
/// above, and this size is the expensive one — what it is here for is the
/// guarantee cell and the cost columns.
#[test]
fn the_dial_ledger_at_twenty_boards() {
    let sweep = [(Ledger::NONE, "off"), (Ledger::after(3), "k=3")];
    let rows = ledger_rows(20, &sweep);
    let pick = |room: &str, label: &str, life: &str| -> &Outcome {
        &rows
            .iter()
            .find(|((rr, rl, rlife), _)| *rr == room && *rl == label && *rlife == life)
            .expect("the row was measured above")
            .1
    };
    for (room, life, bb, disc) in [
        ("empty", "immortal", 20_000, 0),
        ("empty", "45s", 242_126, 0),
        ("phone", "45s", 239_166, 0),
        ("phone", "600s", 38_008, 42),
        ("rig", "45s", 236_596, 0),
        ("rig", "600s", 36_554, 78),
    ] {
        let off = pick(room, "off", life);
        assert_eq!(
            (off.board_links, off.disconnected, off.held_windows),
            (bb, disc, 0),
            "n=20 {room} {life}: the ledger moved a row 311 measured without one"
        );
    }
    assert_ledger_costs_the_room_nothing(20, &rows, &sweep);
    // At twenty boards the phone room is inert to the last dial: the room
    // makes no refused dial at all over the thousand orders, so there is
    // no run of waste for any K to remember, and the shipped row and the
    // ledger row are the same row. Where the ledger does fire at this
    // size — the rooms whose links die young — it costs nothing.
    let (with, off) = (pick("phone", "k=3", "45s"), pick("phone", "off", "45s"));
    assert_eq!(
        (
            with.held_windows,
            with.refused,
            with.board_links,
            with.dials
        ),
        (0, 0, off.board_links, off.dials),
        "n=20 phone 45s: the ledger now has something to remember in a room of twenty, so \
         the inertness this size is quoted for has to be re-measured"
    );
    for (room, life) in [
        ("empty", "45s"),
        ("empty", "600s"),
        ("rig", "45s"),
        ("rig", "600s"),
    ] {
        let (with, off) = (pick(room, "k=3", life), pick(room, "off", life));
        assert_eq!(
            (with.disconnected, with.boardless),
            (off.disconnected, off.boardless),
            "n=20 {room} {life}: the ledger moved connectivity at the size #375's guarantee \
             is stated at, holding {} windows shut",
            with.held_windows
        );
    }
}

/// The ledger's positive controls, in the shape the churn model's, the
/// mortality model's and the static peer's have: the parameter is a
/// parameter, the two things that feed it are shown feeding it
/// separately, and the constants the tables are measured at are the
/// crate's own rather than a second copy of them.
#[test]
fn control_the_dial_ledger_is_a_parameter_and_every_mechanism_fires() {
    // 1. The harness measures the SHIPPED rule, not a restatement of it:
    //    the row labelled `k=3` is `LedgerPolicy::MEASURED` to the
    //    millisecond, and each of the three defaults is the crate
    //    constant it is read off. Three assertions rather than one, so a
    //    drift names which parameter drifted.
    assert_eq!(
        Ledger::after(3).policy(),
        LedgerPolicy::MEASURED,
        "the sweep's middle row is no longer the policy the crate ships"
    );
    assert_eq!(
        (USEFUL_SESSION_MS, LedgerPolicy::USEFUL_SESSION_MS),
        (LedgerPolicy::USEFUL_SESSION_MS, LINK_ABANDONED_MS / 2),
        "the harness's useful-session bound and the crate's have parted"
    );
    assert_eq!(
        (u64::from(Ledger::PAUSE_ROUNDS) * ROUND_MS, DEAD_END_TTL_MS),
        (LedgerPolicy::PAUSE_MS, LedgerPolicy::PAUSE_MS),
        "the pause is meant to be the period the firmware's address table already waits"
    );

    // 2. The two things that feed the ledger, shown feeding it one at a
    //    time, in the two rooms that hold exactly one of them.
    //
    //    A churning peer that only ADVERTISES can never produce a
    //    duplicate refusal — a duplicate needs a link in the other role,
    //    so the room's `refused` column is exactly zero. In that room
    //    with links that never end the ledger fires NOT AT ALL, and the
    //    reason is a phase lock worth knowing about: the top board's
    //    cycle after a rotation is the expiry sweep
    //    ([`LINK_EXPIRY_ROUNDS`], 9 rounds) plus the strict phase
    //    ([`FALLBACK_AFTER_ROUNDS`], 6), so it re-dials 6 rounds into the
    //    peer's 9-round rotation and every session it buys is exactly 3
    //    rounds — exactly [`USEFUL_SESSION_MS`], which belongs to the
    //    useful side. So in the immortal room every wasted dial the
    //    ledger sees is a REFUSAL, and that is what the tables' immortal
    //    rows are measuring.
    let advertiser_immortal = measure(
        3,
        FallbackSpec::Eager,
        TargetChoice::RotatingLast,
        Churn::advertisers(1),
        Mortality::IMMORTAL,
        Statics::NONE,
        Ledger::after(3),
        ConnectFailure::NONE,
    );
    assert_eq!(
        (
            advertiser_immortal.refused,
            advertiser_immortal.held_windows
        ),
        (0, 0),
        "a room whose only churning peer never dials has no refusal to feed the ledger, and \
         its every churn session is exactly one useful-session bound long; if it now fires, \
         one of those two facts has moved and the immortal rows mean something else"
    );
    //    Turn the mortality row on in the same room and the OTHER feed
    //    appears on its own: no refusal anywhere, and sessions that end
    //    below the bound, and the ledger fires on those alone.
    let advertiser_mortal = measure(
        3,
        FallbackSpec::Eager,
        TargetChoice::RotatingLast,
        Churn::advertisers(1),
        Mortality::CAPTURE_SHORT_MODE,
        Statics::NONE,
        Ledger::after(3),
        ConnectFailure::NONE,
    );
    assert_eq!(
        advertiser_mortal.refused, 0,
        "a peer that never dials produced a duplicate refusal, so this row is not the \
         session-length feed on its own"
    );
    assert!(
        advertiser_mortal.held_windows > 0,
        "with no refusal in the room and sessions dying below the bound, the ledger still \
         never fired: the session-length half does not feed the table at all"
    );
    //    And with the same peer dialling, the refusal feed is added to
    //    it: strictly more windows held, same room otherwise.
    let dialling_mortal = measure(
        3,
        FallbackSpec::Eager,
        TargetChoice::RotatingLast,
        Churn::phones(1),
        Mortality::CAPTURE_SHORT_MODE,
        Statics::NONE,
        Ledger::after(3),
        ConnectFailure::NONE,
    );
    assert!(
        dialling_mortal.refused > 0
            && dialling_mortal.held_windows > advertiser_mortal.held_windows,
        "the refusal half added nothing: {} refusals and {} held windows against {} held \
         windows with no refusal in the room",
        dialling_mortal.refused,
        dialling_mortal.held_windows,
        advertiser_mortal.held_windows
    );

    // 3. And the pause is what withholds the dial, not the bookkeeping: a
    //    ledger whose pause is zero records every run exactly as the
    //    measured one does and holds nothing, so the room is the shipped
    //    room dial for dial.
    let no_pause = measure(
        3,
        FallbackSpec::Eager,
        TargetChoice::RotatingLast,
        Churn::phones(1),
        Mortality::IMMORTAL,
        Statics::NONE,
        Ledger::with_pause(3, 0),
        ConnectFailure::NONE,
    );
    let off = measure(
        3,
        FallbackSpec::Eager,
        TargetChoice::RotatingLast,
        Churn::phones(1),
        Mortality::IMMORTAL,
        Statics::NONE,
        Ledger::NONE,
        ConnectFailure::NONE,
    );
    assert_eq!(
        (
            no_pause.held_windows,
            no_pause.dials,
            no_pause.board_links,
            no_pause.refused
        ),
        (0, off.dials, off.board_links, off.refused),
        "a ledger with a zero pause changed the room, so something other than the pause is \
         withholding dials"
    );
}

/// The static peer's positive controls, in the shape the churn model's
/// and the mortality model's have: the parameter is a parameter, and each
/// of its mechanisms is shown firing before the tables above may be read
/// as measurements.
#[test]
fn control_the_static_peer_is_a_parameter_and_dials_like_the_capture() {
    // 1. Its zero moves nothing, at the sizes 306 published and on the
    //    exact cells it published. Equalities: the peer draws from a
    //    stream of its own.
    for (n, life, mortality, bb, disc) in [
        (10usize, "45s", Mortality::CAPTURE_SHORT_MODE, 118_040, 0),
        (20, "45s", Mortality::CAPTURE_SHORT_MODE, 239_166, 0),
        (20, "600s", Mortality::CAPTURE_TAIL, 38_008, 42),
    ] {
        let outcome = measure(
            n,
            FallbackSpec::Eager,
            TargetChoice::RotatingLast,
            Churn::phones(1),
            mortality,
            Statics::NONE,
            Ledger::NONE,
            ConnectFailure::NONE,
        );
        assert_eq!(
            (
                outcome.board_links,
                outcome.disconnected,
                outcome.static_links
            ),
            (bb, disc, 0),
            "n={n} life={life}: `Statics::NONE` moved a cell 306 measured"
        );
    }

    // 2. It accepts dials: the room's boards reach it, and every one of
    //    those links is a dial the boards would otherwise have spent on
    //    each other or on the phone.
    let accepting = measure(
        3,
        FallbackSpec::Eager,
        TargetChoice::RotatingLast,
        Churn::phones(1),
        Mortality::CAPTURE_SHORT_MODE,
        Statics::accepting(1),
        Ledger::NONE,
        ConnectFailure::NONE,
    );
    assert!(
        accepting.static_links > 0,
        "no board ever dialled the static peer, so the kind is inert and every row with it is \
         a row without it"
    );

    // 3. It DIALS, which is the half the capture proves (84 links it
    //    initiated across the three boards, 76 of them to `feld-pocket`,
    //    which never dialled it once). The switch is a switch: with it
    //    off no board ever holds an incoming link from the peer, with it
    //    on they do.
    for (statics, expected) in [(Statics::accepting(1), false), (Statics::rig(1), true)] {
        let mut seen = false;
        for seed in 0..50 {
            let sim = run_sim(
                3,
                0xB1E5_0000 + seed,
                FallbackSpec::Eager,
                TargetChoice::RotatingLast,
                Churn::NONE,
                Mortality::CAPTURE_SHORT_MODE,
                statics,
                Ledger::NONE,
                ConnectFailure::NONE,
            );
            seen |= sim
                .boards
                .iter()
                .any(|b| b.incoming.iter().any(|l| l.peer >= 3));
        }
        assert_eq!(
            seen, expected,
            "{statics:?}: the peer's own dialling half does not follow its switch"
        );
    }

    // 4. A static address cannot produce the duplicate the rotation
    //    produces: the §4.5 exclusion catches a peer we already hold a
    //    link with before the dial, so no dial to it is ever refused
    //    post-connect. That is the one structural difference from the
    //    phone, and it is measured rather than asserted from the code.
    let no_phone = measure(
        3,
        FallbackSpec::Eager,
        TargetChoice::RotatingLast,
        Churn::NONE,
        Mortality::CAPTURE_SHORT_MODE,
        Statics::rig(1),
        Ledger::NONE,
        ConnectFailure::NONE,
    );
    assert!(
        no_phone.static_links > 0 && no_phone.refused == 0,
        "{} dials to a static peer were refused as duplicates; without a rotation there is no \
         second address for the identity to arrive under",
        no_phone.refused
    );

    // 5. And 303's tier is still worth zero with the fourth kind in the
    //    room: it advertises a record, but not a peripheral-only one
    //    (`caps=0x1e` and `0x1c` in the capture, bit 0 clear).
    assert_eq!(
        no_phone.offers_cannot_dial, 0,
        "an offer reached the cannot-dial-us tier: something in the model now advertises \
         PERIPHERAL_ONLY and the tier rows have to be measured for real"
    );
    // A three-board room with no phone in it offers far less than the
    // steady-state table's millions — measured 51 474 over the thousand
    // orders — so the bound says "enough windows to see the tier fire if
    // it could", not "as many as that table".
    assert!(
        no_phone.offers > 10_000,
        "the tier census saw only {} offers with a static peer in the room, too few to say the \
         tier never fired",
        no_phone.offers
    );
}

/// The mortality model's positive controls, in the shape the churn
/// model's has: the parameter is a parameter (its zero changes nothing),
/// and each of its mechanisms is shown firing once before the
/// steady-state table above may be read as a measurement.
#[test]
fn control_the_mortality_model_is_a_parameter_and_frees_both_slots() {
    // 1. The distribution is the capture's, stated from the constants it
    //    was read against, so the model's clock is not free-floating.
    assert_eq!(
        Mortality::CAPTURE_SHORT_MODE.mean_ms,
        LINK_TIMEOUT_MS,
        "the short mode is the registry's own expiry bound"
    );
    assert_eq!(
        rounds(Mortality::CAPTURE_SHORT_MODE.mean_ms),
        9,
        "45 s at 5 s a round"
    );
    assert!(!Mortality::IMMORTAL.kills());
    assert!(Mortality::CAPTURE_SHORT_MODE.kills() && Mortality::CAPTURE_TAIL.kills());
    // A drawn lifetime is at least one round and the mean is the stated
    // one: 10 000 draws, within 3 % of 9 rounds.
    let mut stream = 0x1234_5678_9ABC_DEF1u64;
    let mut total = 0u64;
    for _ in 0..10_000 {
        let life = Mortality::CAPTURE_SHORT_MODE.draw_lifetime(&mut stream);
        assert!(life >= 1, "a link died in the round it formed");
        total += u64::from(life);
    }
    let mean = total as f64 / 10_000.0;
    assert!(
        (mean - 9.5).abs() < 0.3,
        "the geometric draw's mean is {mean:.2} rounds, not the 9.5 the ceiling of a 9-round \
         exponential gives — the stated distribution and the drawn one have parted"
    );

    // 2. `Mortality::IMMORTAL` is the pre-steady-state simulation order
    //    by order, not just in aggregate: nothing dies, and with nobody
    //    churning no slot is ever freed, so there is no steady state to
    //    read and the `freed%` column is `-` rather than 0.
    for n in [10usize, 20] {
        for seed in 0..50 {
            let sim = run_sim(
                n,
                0xB1E5_0000 + seed,
                FallbackSpec::Eager,
                TargetChoice::RotatingLast,
                Churn::NONE,
                Mortality::IMMORTAL,
                Statics::NONE,
                Ledger::NONE,
                ConnectFailure::NONE,
            );
            assert_eq!(sim.tally.deaths, 0);
            assert_eq!(sim.tally.refill_dials, 0);
            assert_eq!(sim.tally.dials, sim.tally.board_links);
        }
    }
    // The aggregate half of the same claim is the two tables above: every
    // row they pin is pinned as an EQUALITY and measured with
    // `Mortality::IMMORTAL`, so a mortality path that took one draw from
    // any other stream would have moved them.

    // 3. A death frees BOTH slots. The run-wide invariant in `run_sim`
    //    catches a half-freed link on every order; this is the direct
    //    reading, on one order, of the thing that invariant protects: a
    //    board that has lost links is back under the slot limit and back
    //    in the scan, so the boards keep forming links all run long
    //    instead of once.
    let sim = run_sim(
        10,
        0xB1E5_0000,
        FallbackSpec::Eager,
        TargetChoice::RotatingLast,
        Churn::NONE,
        Mortality::CAPTURE_SHORT_MODE,
        Statics::NONE,
        Ledger::NONE,
        ConnectFailure::NONE,
    );
    assert!(
        sim.tally.deaths > 20,
        "only {} board-to-board links died over {HORIZON_ROUNDS} rounds at a {} ms mean \
         lifetime — the sweep is not firing",
        sim.tally.deaths,
        Mortality::CAPTURE_SHORT_MODE.mean_ms
    );
    assert!(
        sim.tally.board_links > 10,
        "the room formed {} board links in total, so nothing re-linked after a death",
        sim.tally.board_links
    );
    for (i, board) in sim.boards.iter().enumerate() {
        assert!(
            board.incoming.len() <= PERIPH_SLOTS,
            "board {i} ended over the incoming slot limit"
        );
    }

    // 4. The one direction that must NOT be read as good news, the
    //    mortality version of the churn model's confound: a room whose
    //    links die forms far MORE board-to-board links than one whose
    //    links stand (113 300 against 10 000 per 1000 orders at n=10),
    //    and that is a count of formations, not of connectivity. The
    //    split-graph column is the connectivity reading, and it is taken
    //    as a snapshot at the horizon.
    let immortal = measure(
        10,
        FallbackSpec::Eager,
        TargetChoice::RotatingLast,
        Churn::NONE,
        Mortality::IMMORTAL,
        Statics::NONE,
        Ledger::NONE,
        ConnectFailure::NONE,
    );
    let mortal = measure(
        10,
        FallbackSpec::Eager,
        TargetChoice::RotatingLast,
        Churn::NONE,
        Mortality::CAPTURE_SHORT_MODE,
        Statics::NONE,
        Ledger::NONE,
        ConnectFailure::NONE,
    );
    assert!(
        mortal.board_links > immortal.board_links * 5,
        "the mortal room formed {} board links against the immortal room's {}; if that ratio \
         has collapsed, the `bb` column no longer means formations and the table's reading \
         of it is stale",
        mortal.board_links,
        immortal.board_links
    );
    assert_eq!(
        (immortal.disconnected, mortal.disconnected),
        (0, 0),
        "a room of boards with no churning peer in it stopped converging"
    );
}

/// The shipped configuration at both sizes: no board is EVER left
/// without a BLE link, and no arrival order ends disconnected — every
/// order forms one connected component. Eager is safe to ship because
/// the doomed dial that forced part 2's quiet spec is closed at its
/// root: the §4.5 exclusion keeps a live connection's address out of
/// the scanner and the dead-end table backs off a fallback target that
/// will not connect. The quiet rows in the module table stay as the
/// record of what the suspension cost (28 and 78 all-linked splits per
/// 1000).
///
/// BOTH windows are replayed: `LowestEligible` is what item 2 shipped,
/// `MostFreeSlots` what item 3 ships. Item 3 refines the ORDER inside a
/// class, so every arrival order must still converge — this is the
/// no-regression assertion the batch is held to, and it runs over the
/// same 2000 replayed orders that established the item 2 result rather
/// than a new instrument.
///
/// It is an EMPTY room, and since #412 that is stated rather than
/// implied: the claim was always about a room of boards, and the churn
/// table is where a room with a phone in it is answered for.
#[test]
fn the_shipped_config_connects_every_order_and_strands_nobody() {
    for choice in [
        TargetChoice::LowestEligible,
        TargetChoice::MostFreeSlots,
        TargetChoice::RotatingLast,
    ] {
        for n in [10usize, 20] {
            let mut split = 0usize;
            for seed in 0..ORDERS {
                let sim = run_sim(
                    n,
                    0xB1E5_0000 + seed,
                    FallbackSpec::Eager,
                    choice,
                    Churn::NONE,
                    Mortality::IMMORTAL,
                    Statics::NONE,
                    Ledger::NONE,
                    ConnectFailure::NONE,
                );
                for (i, b) in sim.boards.iter().enumerate() {
                    assert!(
                        b.outgoing.is_some() || !b.incoming.is_empty(),
                        "board {i} ended with no BLE link at n={n}, seed {seed}, {choice:?}"
                    );
                }
                if !is_connected(&sim.boards) {
                    split += 1;
                }
            }
            assert_eq!(
                split, 0,
                "eager/{choice:?} left {split} of {ORDERS} orders disconnected at n={n}"
            );
        }
    }
}

/// The control: identical harness, identical seeds, fallback off — the
/// strict sort alone must strand boards for a large share of orders, or
/// the fallback tests above prove nothing about the fallback. The
/// measured rate is 210/1000 disconnected orders (the issue's Monte
/// Carlo says 21 %), 84 of which leave some board with no link at all.
#[test]
fn control_the_strict_rule_alone_disconnects_a_fifth_of_the_orders() {
    let outcome = measure(
        10,
        FallbackSpec::Off,
        TargetChoice::FirstSeen,
        Churn::NONE,
        Mortality::IMMORTAL,
        Statics::NONE,
        Ledger::NONE,
        ConnectFailure::NONE,
    );
    assert!(
        outcome.disconnected >= 100,
        "the strict rule connected almost every order ({}/{ORDERS} lost); \
         the control lost its teeth",
        outcome.disconnected
    );
    assert!(
        outcome.linkless >= 50,
        "strict orders with a fully linkless board: {}/{ORDERS}",
        outcome.linkless
    );
}

// ── #412 part 1: the scarce slot goes to the peer that cannot dial ──
//
// Everything above measures a room over a thousand arrival orders. The
// cells below are rooms of two, replayed in both arrival orders, and
// they ask the one question the Monte Carlo cannot: with the outgoing
// slot free and two peers permitted, WHICH one is it spent on? The
// design comment on #412 (2026-09-16) answers it first of four: the
// slot goes to the peer whose advertisement says it cannot initiate,
// because a central-capable peer comes to us and lands in one of the
// three incoming slots, which cost nothing.
//
// The Monte Carlo cannot see this because no peer in it is ever
// peripheral-only: boards advertise `LOCAL_CAPS = 0` and the churning
// peer carries no record at all. That is also why every pinned number
// above is unmoved by part 1 — the preference splits a class whose
// members the harness never produces — and the tables holding as
// equalities is the proof of it, not a hope.

/// A board's own capability byte: the firmware's `LOCAL_CAPS` and
/// lnsd's, dual-role since #255 phase B, so bit 0 is clear.
const DUAL_ROLE: u8 = 0;

/// Static random addresses (`11` on top, Core Spec Vol 6 Part B
/// §1.3.2.2) — what every board in the room has — in ascending order,
/// so a cell can put a peer on either side of the v2.2 sort.
const OUR_BOARD: u64 = 0xC000_0000_0001;
const BOARD_A: u64 = 0xC000_0000_0002;
const BOARD_B: u64 = 0xC000_0000_0003;

/// One resolvable private address (`01` on top): an Android Columba,
/// central-capable, carrying no v0.3.0 record at all.
const PHONE_ADDR: u64 = 0x4A1B_2C3D_4E5F;

/// How many rotations a cell replays. The capture's period is
/// [`CHURN_ROTATE_MS`] and the churn table's horizon is
/// [`HORIZON_ROUNDS`], so this is the same number of redraws the
/// measurement above runs to, stated from the same two constants.
const ROTATIONS: u32 = HORIZON_ROUNDS / rounds(CHURN_ROTATE_MS);

/// One advertiser in a one-window room: the label the assertions name
/// it by, the address it is advertising under right now, and the
/// v0.3.0 capability byte it carries. `None` is a peer with no record
/// at all — per §3.2 full capability, per the window "all slots free".
#[derive(Debug, Clone, Copy)]
struct Advertiser {
    label: &'static str,
    addr: u64,
    caps: Option<u8>,
}

/// A capability record stating a role and a free-slot count, built by
/// the same [`with_free_slots`] the advertiser builds it with, so a
/// cell cannot state a byte the wire could not carry.
const fn record(caps: u8, free: u8) -> Option<u8> {
    Some(with_free_slots(caps, free))
}

/// One scan window of a board at `own_addr` over `room`, with the REAL
/// rule and the REAL table: every advertiser gets a [`should_initiate`]
/// verdict, the eligible ones enter the window in the order listed, and
/// the window elects the dial. `None` is "the rule permitted nobody" —
/// the searching state, not a choice.
///
/// The capability byte goes in and a dial comes out, which is what
/// makes these rooms and not table lookups: a preference that did not
/// survive the decode would fail here.
fn dial_from(own_addr: u64, room: &[Advertiser], mode: ScanMode) -> Option<&'static str> {
    let mut window: CandidateTable<&'static str, WINDOW_CANDIDATES> = CandidateTable::new();
    for peer in room {
        let decision = should_initiate(DUAL_ROLE, own_addr, peer.caps, peer.addr, mode);
        window.offer(
            peer.addr,
            decision,
            peer.caps.and_then(free_slots),
            peer.label,
        );
    }
    window.into_best().map(|(_, _, label)| label)
}

/// The same room heard both ways round. Which advertiser the radio
/// happened to hear first may not decide a strict choice — the one
/// place it may is the fallback class's own tie-break, and no cell
/// below puts two fallback candidates against each other.
fn dial_either_way(own_addr: u64, room: [Advertiser; 2], mode: ScanMode) -> &'static str {
    let forward = dial_from(own_addr, &room, mode);
    let reversed = dial_from(own_addr, &[room[1], room[0]], mode);
    assert_eq!(
        forward, reversed,
        "the choice followed the arrival order: {room:?}"
    );
    forward.expect("the room holds a peer the rule permits")
}

/// The mechanism the steady-state table found, as one window (#412
/// steady state). NOT a property anybody wants — a record of why the
/// shipped fallback order cannot defend the freed slot in a small room,
/// so that the fix which changes it has something to flip.
///
/// The key's terms are (tier, free-slot deficit, rotating group, address
/// or sighting) and the DEFICIT sits above the group. A phone advertises
/// no capability record at all, which reads as "all slots free" and
/// deficits by zero; a board that has spent even one of its three
/// incoming slots deficits by one. So in any fallback window where every
/// board on offer is carrying at least one incoming link, the phone wins
/// on the second term before the rotating-last order is ever consulted.
///
/// That is the rig's room: three boards, each holding an incoming link
/// from the others, and the highest-addressed one — `feld-t114`, dialled
/// by both the others — has no strict candidate at all. Its slot went to
/// the phone 7 times out of 7, and the steady-state table reproduces
/// 100 % under BOTH fallback orders for this reason and not for lack of
/// a board to dial (the `solo%` column is 3 % there).
#[test]
fn a_board_that_has_spent_an_incoming_slot_loses_the_fallback_to_a_silent_phone() {
    let spent = Advertiser {
        label: "board holding one incoming link",
        addr: BOARD_A,
        caps: record(DUAL_ROLE, PERIPH_SLOTS as u8 - 1),
    };
    let phone = Advertiser {
        label: "phone",
        addr: PHONE_ADDR,
        caps: None,
    };
    assert_eq!(
        dial_either_way(BOARD_B, [spent, phone], ScanMode::Fallback),
        "phone",
        "the deficit term no longer outranks the rotating-last order — if this is now the \
         board, the steady-state table's 100 % rows at n=3 are stale and have to be re-measured"
    );
    // The same window with a board that has spent nothing: then the
    // deficits tie and the rotating-last order decides, which is the
    // behaviour #412 shipped and which still holds.
    let free = Advertiser {
        label: "board with every slot free",
        addr: BOARD_A,
        caps: record(DUAL_ROLE, PERIPH_SLOTS as u8),
    };
    assert_eq!(
        dial_either_way(BOARD_B, [free, phone], ScanMode::Fallback),
        "board with every slot free",
        "the rotating-last order lost a window it decides today"
    );
}

/// #412 part 1, the cell that was red before the preference existed:
/// two peers the rule permits, one of which can dial us back.
///
/// Both terms that decided this before part 1 point the wrong way on
/// purpose. The central-capable neighbour advertises MORE free slots,
/// so it won the #375 item 3 preference, and it holds the LOWER
/// address, so it would have won the term after that too. It is also
/// the peer that needs us least: it sorts above us, so it will dial us
/// and land in one of three incoming slots. The peripheral-only board
/// has no way to reach us at all, and we have one outgoing slot.
#[test]
fn the_window_prefers_the_peer_that_cannot_dial_us_back() {
    let cannot = Advertiser {
        label: "peripheral-only board",
        addr: BOARD_B,
        caps: record(CAP_PERIPHERAL_ONLY, 2),
    };
    let can = Advertiser {
        label: "dual-role board",
        addr: BOARD_A,
        caps: record(DUAL_ROLE, 3),
    };
    // The verdicts the two get are both strict, which is why the class
    // term alone never separated them.
    assert_eq!(
        should_initiate(
            DUAL_ROLE,
            OUR_BOARD,
            cannot.caps,
            cannot.addr,
            ScanMode::Strict
        ),
        ConnectDecision::InitiatePeripheralOnlyPeer
    );
    assert_eq!(
        should_initiate(DUAL_ROLE, OUR_BOARD, can.caps, can.addr, ScanMode::Strict),
        ConnectDecision::InitiateLowerAddress
    );
    assert_eq!(
        dial_either_way(OUR_BOARD, [cannot, can], ScanMode::Strict),
        "peripheral-only board"
    );
    // And with the slot counts equal, so that the cell above cannot be
    // read as "the emptier peer lost by accident".
    let can = Advertiser {
        caps: record(DUAL_ROLE, 2),
        ..can
    };
    assert_eq!(
        dial_either_way(OUR_BOARD, [cannot, can], ScanMode::Strict),
        "peripheral-only board"
    );
}

/// The room the design comment names: one central-capable phone and
/// one peripheral-only board.
///
/// On a BOARD this half was already held, and by a different term. A
/// resolvable private address is below every static random one
/// (`peer.rs`), so the v2.2 sort never permits a board to dial a phone
/// at all: the phone can only ever be a FALLBACK candidate, which the
/// class term has outranked since #375 item 2. The cell states that
/// rather than assuming it, because part 1 must not be credited with
/// it.
///
/// The half part 1 does decide is the same room seen by a scanner that
/// sorts BELOW the phone — lnsd on a host adapter with a low public
/// address, running this very table. There the phone IS a strict
/// candidate, it advertises no slot count so it ranks as empty, and
/// before part 1 it took the slot from the peer that has no other way
/// in.
#[test]
fn a_phone_does_not_take_the_slot_from_a_board_that_cannot_dial() {
    let phone = Advertiser {
        label: "phone",
        addr: PHONE_ADDR,
        caps: None,
    };
    let board = Advertiser {
        label: "peripheral-only board",
        addr: BOARD_A,
        caps: record(CAP_PERIPHERAL_ONLY, 1),
    };
    for mode in [ScanMode::Strict, ScanMode::Fallback] {
        assert_eq!(
            should_initiate(
                DUAL_ROLE,
                OUR_BOARD,
                phone.caps,
                phone.addr,
                ScanMode::Strict
            ),
            ConnectDecision::WaitPeerHasLowerAddress,
            "a board never gets a strict verdict for an RPA"
        );
        assert_eq!(
            dial_either_way(OUR_BOARD, [phone, board], mode),
            "peripheral-only board"
        );
    }
    let low_host = PHONE_ADDR - 1;
    assert!(
        should_initiate(
            DUAL_ROLE,
            low_host,
            phone.caps,
            phone.addr,
            ScanMode::Strict
        )
        .initiate(),
        "the cell needs a scanner the sort sends AT the phone"
    );
    assert_eq!(
        dial_either_way(low_host, [phone, board], ScanMode::Strict),
        "peripheral-only board"
    );
}

/// Two peers that both cannot dial: the order between them is the one
/// they had before part 1, because nothing about them differs on the
/// new term. Emptiest first (#375 item 3), equal counts by the lowest
/// address (#375 item 2).
#[test]
fn two_peers_that_cannot_dial_keep_the_order_they_had() {
    let fuller = Advertiser {
        label: "one slot left",
        addr: BOARD_A,
        caps: record(CAP_PERIPHERAL_ONLY, 1),
    };
    let emptier = Advertiser {
        label: "three slots left",
        addr: BOARD_B,
        caps: record(CAP_PERIPHERAL_ONLY, 3),
    };
    assert_eq!(
        dial_either_way(OUR_BOARD, [fuller, emptier], ScanMode::Strict),
        "three slots left"
    );
    let low = Advertiser {
        label: "low",
        addr: BOARD_A,
        caps: record(CAP_PERIPHERAL_ONLY, 2),
    };
    let high = Advertiser {
        label: "high",
        addr: BOARD_B,
        caps: record(CAP_PERIPHERAL_ONLY, 2),
    };
    assert_eq!(
        dial_either_way(OUR_BOARD, [low, high], ScanMode::Strict),
        "low"
    );
}

/// A rotating peer redraws its address, never its standing. A fresh
/// draw of the whole 46-bit space is a fresh chance to win an ordering
/// term — that is #412's mechanism — and part 1 must not open a new
/// door to it. Every redraw the churn horizon covers, against a board
/// of each kind, in both scan modes.
#[test]
fn a_redrawn_address_never_takes_the_slot_from_a_board() {
    let boards = [
        Advertiser {
            label: "board",
            addr: BOARD_A,
            caps: record(CAP_PERIPHERAL_ONLY, 1),
        },
        Advertiser {
            label: "board",
            addr: BOARD_A,
            caps: record(DUAL_ROLE, 1),
        },
    ];
    let mut rng = 0x0412_0412_0412_0412;
    let mut phone = Advertiser {
        label: "phone",
        addr: PHONE_ADDR,
        caps: None,
    };
    for _ in 0..ROTATIONS {
        // A resolvable private address: `01` on top, the rest redrawn.
        phone.addr = (next_rand(&mut rng) & 0x3FFF_FFFF_FFFF) | (0b01 << 46);
        for board in boards {
            for mode in [ScanMode::Strict, ScanMode::Fallback] {
                assert_eq!(
                    dial_either_way(OUR_BOARD, [phone, board], mode),
                    "board",
                    "rotation to {:012x} won a term it should not have",
                    phone.addr
                );
            }
        }
    }
    // A preference inside what the rule already permits, never an
    // exclusion: with nobody else advertising, the phone is the dial.
    assert_eq!(
        dial_from(OUR_BOARD, &[phone], ScanMode::Fallback),
        Some("phone")
    );
}

/// The preference is for the peer that cannot dial US, not for one
/// that cannot take the dial either. A peer advertising zero free
/// incoming slots refuses the connection when it lands, and the
/// deficit term has sorted it last inside its class since #375 item 3;
/// promoting it over every reachable peer would have inverted that.
///
/// Silence is not zero, here as everywhere: a peripheral-only peer that
/// said nothing about its slots is ranked as having them all, so it
/// keeps the preference.
#[test]
fn a_peer_that_cannot_dial_and_has_no_room_is_not_promoted() {
    let full = Advertiser {
        label: "peripheral-only, full",
        addr: BOARD_A,
        caps: record(CAP_PERIPHERAL_ONLY, 0),
    };
    let reachable = Advertiser {
        label: "dual-role, room left",
        addr: BOARD_B,
        caps: record(DUAL_ROLE, 1),
    };
    assert_eq!(
        dial_either_way(OUR_BOARD, [full, reachable], ScanMode::Strict),
        "dual-role, room left"
    );
    // Still a candidate, and still the dial when it is the only one: a
    // count that may be stale must not cost a peer its only chance.
    assert_eq!(
        dial_from(OUR_BOARD, &[full], ScanMode::Strict),
        Some("peripheral-only, full")
    );
    let silent = Advertiser {
        label: "peripheral-only, silent",
        caps: Some(CAP_PERIPHERAL_ONLY),
        ..full
    };
    assert_eq!(
        silent.caps.and_then(free_slots),
        None,
        "no count was stated"
    );
    assert_eq!(
        dial_either_way(OUR_BOARD, [silent, reachable], ScanMode::Strict),
        "peripheral-only, silent"
    );
}

/// The fourth kind in one window (#412 steady state 2): the mechanism
/// that emptied the phone's column on the rig, with nothing in it but the
/// real rule and the real table.
///
/// The three advertisers are the rig's room as the capture describes it:
/// a board that has spent one of its three incoming slots (so it deficits
/// by one), the phone with no capability record at all (deficit zero,
/// rotating address), and the solar node — a STATIC address with a record
/// stating three free slots, which is what `caps=0x1e free_slots=3` on
/// 1835 of its `BLE_SCAN_DECISION` lines says. Under the shipped order it
/// wins the deficit term against the board and the rotating-group term
/// against the phone, so it takes the dial, and that is the whole of the
/// rig's post-2026-09-25 ledger.
#[test]
fn a_static_peer_with_every_slot_free_takes_the_fallback_from_board_and_phone() {
    let spent = Advertiser {
        label: "board holding one incoming link",
        addr: BOARD_A,
        caps: record(DUAL_ROLE, PERIPH_SLOTS as u8 - 1),
    };
    let phone = Advertiser {
        label: "phone",
        addr: PHONE_ADDR,
        caps: None,
    };
    // Below every board, as `d916e2923ed2` was below all three — and
    // still a static random address, so it is not in the rotating group.
    let solar = Advertiser {
        label: "static peer, every slot free",
        addr: OUR_BOARD,
        caps: record(DUAL_ROLE, PERIPH_SLOTS as u8),
    };
    assert_eq!(
        dial_from(BOARD_B, &[spent, phone, solar], ScanMode::Fallback),
        Some("static peer, every slot free"),
        "the static peer lost the rig's own window; the steady-state-2 table's reading of the \
         capture is then wrong"
    );
    // Without it, the same window is 306's finding: the phone.
    assert_eq!(
        dial_from(BOARD_B, &[spent, phone], ScanMode::Fallback),
        Some("phone"),
        "306's mechanism cell has changed under this order"
    );
}

/// One window elected under a stated policy, through the same [`elect`]
/// the simulation calls — so a cell can put the two candidate keys of
/// #412 steady state 2 against each other on one advertisement set.
fn elect_from(own_addr: u64, room: &[Advertiser], choice: TargetChoice) -> &'static str {
    let offers: Vec<(usize, u64, ConnectDecision, Option<u8>)> = room
        .iter()
        .enumerate()
        .filter_map(|(k, peer)| {
            let decision = should_initiate(
                DUAL_ROLE,
                own_addr,
                peer.caps,
                peer.addr,
                ScanMode::Fallback,
            );
            decision
                .initiate()
                .then_some((k, peer.addr, decision, peer.caps.and_then(free_slots)))
        })
        .collect();
    let mut side_ledger = Tally::default();
    room[elect(choice, &offers, &mut side_ledger)].label
}

/// The two candidate keys ARE two different policies, which the Monte
/// Carlo cannot show: every room it can build has one peer that is both
/// recordless AND rotating (the phone), so both keys demote the same
/// peer and every cell of the tables is identical. Separating them takes
/// a peer in one set and not the other, and the two below are exactly
/// those:
///
/// - a rotating peer that DOES advertise a record — an Android Columba
///   that adopted v0.3.0's capability byte, which none does today;
/// - a static peer that advertises NOTHING — an older board, or another
///   implementation's node.
///
/// The cell is the reason item 4's sentence cannot choose between the two
/// on the tables alone.
#[test]
fn the_two_candidate_keys_are_two_different_policies() {
    let spent = Advertiser {
        label: "board holding one incoming link",
        addr: BOARD_A,
        caps: record(DUAL_ROLE, PERIPH_SLOTS as u8 - 1),
    };
    // A phone that states three free slots: rotating, but not silent.
    let talking_phone = Advertiser {
        label: "rotating peer with a record",
        addr: PHONE_ADDR,
        caps: record(DUAL_ROLE, PERIPH_SLOTS as u8),
    };
    assert_eq!(
        elect_from(BOARD_B, &[spent, talking_phone], TargetChoice::RotatingLast),
        "rotating peer with a record",
        "the shipped key's deficit term no longer outranks its group term"
    );
    assert_eq!(
        elect_from(
            BOARD_B,
            &[spent, talking_phone],
            TargetChoice::GroupAboveDeficit
        ),
        "board holding one incoming link",
        "ranking the group above the deficit did not demote a rotating peer that had stated a \
         better slot count"
    );
    assert_eq!(
        elect_from(
            BOARD_B,
            &[spent, talking_phone],
            TargetChoice::SilenceIsFull
        ),
        "rotating peer with a record",
        "the silence-is-full key demoted a peer that was not silent"
    );

    // And the mirror: a static peer that says nothing at all.
    let silent_board = Advertiser {
        label: "static peer with no record",
        addr: OUR_BOARD,
        caps: None,
    };
    assert_eq!(
        elect_from(BOARD_B, &[spent, silent_board], TargetChoice::RotatingLast),
        "static peer with no record",
        "silence stopped reading as `all slots free` under the shipped key"
    );
    assert_eq!(
        elect_from(
            BOARD_B,
            &[spent, silent_board],
            TargetChoice::GroupAboveDeficit
        ),
        "static peer with no record",
        "the group term demoted a peer whose address does not rotate"
    );
    assert_eq!(
        elect_from(BOARD_B, &[spent, silent_board], TargetChoice::SilenceIsFull),
        "board holding one incoming link",
        "the silence-is-full key still let a recordless peer deficit by zero"
    );
}

/// The three failure levels every re-measured table below is read at.
const FAILURES: [(ConnectFailure, &str); 3] = [
    (ConnectFailure::NONE, "off"),
    (ConnectFailure::CAPTURE, "capture"),
    (ConnectFailure::CAPTURE_STRANGERS, "strangers"),
];

/// #412, the instrument's first limit: 306's steady-state rows and 311's
/// static-peer rows re-measured with [`ConnectFailure`] on.
///
/// The room is the rig's — three boards and one dialling phone — with
/// and without the solar node, under the address order the shipped one
/// replaced and under the shipped one. Every row is measured at all
/// three failure levels, `off` first, so the delta in `bb`, `d/use` and
/// `disc` is read off the same line.
#[test]
fn the_dial_that_fails_to_connect_re_measures_the_steady_state() {
    println!(
        "#412 — 306's and 311's rooms with a dial that can fail, per {ORDERS} orders. \
         `fail%` is the share of the room's dials that never became a link."
    );
    println!(
        "{:<5} {:<19} {:>8} {:>10} {:>7} {:>7} {:>7} {:>7} {:>6} {:>7} {:>6} {:>5}",
        "room",
        "policy",
        "life",
        "failure",
        "fail%",
        "freed%",
        "stat%",
        "top%",
        "sb",
        "bb",
        "d/use",
        "disc"
    );
    let mut rows = Vec::new();
    for (statics, room) in [(Statics::NONE, "none"), (Statics::rig(1), "rig")] {
        for (choice, policy) in [
            (TargetChoice::MostFreeSlots, "eager/mostfree"),
            (TargetChoice::RotatingLast, "eager/rotatinglast"),
        ] {
            for (mortality, life) in LIFETIMES {
                for (failure, level) in FAILURES {
                    let outcome = measure(
                        3,
                        FallbackSpec::Eager,
                        choice,
                        Churn::phones(1),
                        mortality,
                        statics,
                        Ledger::NONE,
                        failure,
                    );
                    println!(
                        "{room:<5} {policy:<19} {life:>8} {level:>10} {:>7} {:>7} {:>7} {:>7} \
                         {:>6} {:>7} {:>6} {:>5}",
                        Ratio(outcome.share_of_dials_that_failed()),
                        Ratio(outcome.share_of_freed_slots_to_churn()),
                        Ratio(outcome.share_of_top_freed_slots_to_static()),
                        Ratio(outcome.share_of_top_freed_slots_to_churn()),
                        outcome.static_links,
                        outcome.board_links,
                        Ratio(outcome.dials_per_useful()),
                        outcome.disconnected,
                    );
                    rows.push(((room, policy, life, level), outcome));
                }
            }
        }
    }
    let pick = |room: &str, policy: &str, life: &str, level: &str| -> &Outcome {
        &rows
            .iter()
            .find(|((rr, rp, rl, rf), _)| {
                *rr == room && *rp == policy && *rl == life && *rf == level
            })
            .expect("the row was measured above")
            .1
    };

    // The parameter is a parameter. Every `off` row is 306's and 311's
    // row to the link, and no dial in it fails — equalities, because the
    // failure stream is never read at `ConnectFailure::NONE`.
    for (room, policy, life, bb, sb, disc) in [
        ("none", "eager/mostfree", "immortal", 2040, 0, 0),
        ("none", "eager/mostfree", "45s", 26_790, 0, 0),
        ("none", "eager/mostfree", "600s", 3904, 0, 0),
        ("none", "eager/rotatinglast", "45s", 26_790, 0, 0),
        ("rig", "eager/mostfree", "45s", 26_734, 98, 0),
        ("rig", "eager/rotatinglast", "immortal", 2090, 738, 0),
        ("rig", "eager/rotatinglast", "45s", 26_840, 4640, 0),
        ("rig", "eager/rotatinglast", "600s", 3976, 1472, 22),
    ] {
        let off = pick(room, policy, life, "off");
        assert_eq!(
            (
                off.board_links,
                off.static_links,
                off.disconnected,
                off.failed
            ),
            (bb, sb, disc, 0),
            "{room} {policy} {life}: the failure model moved a row measured without one"
        );
    }

    // 1. The rig's own reading — `feld-t114`'s freed outgoing slot going
    //    to the phone 7 times out of 7 — is reproduced at EVERY failure
    //    level in the room 306 read it in, which is the room without the
    //    solar node. The instrument's first limit was not what put it at
    //    100 %.
    for policy in ["eager/mostfree", "eager/rotatinglast"] {
        for (_, life) in LIFETIMES {
            for (_, level) in FAILURES {
                assert_eq!(
                    pick("none", policy, life, level).share_of_top_freed_slots_to_churn(),
                    Some(100.0),
                    "none {policy} {life} {level}: the top board's freed slot left the phone"
                );
            }
        }
    }

    // 2. 311's headline does NOT survive at the capture's own static
    //    rate, and the mechanism is the asymmetry between the two kinds:
    //    the peer the shipped order prefers has a static address and its
    //    dials fail a third of the time, while the peer it demotes has a
    //    rotating one and, in the population that reaches a Reticulum
    //    service at all, never fails. The phone takes the top board's
    //    freed slot 0.32 % of the time with every dial connecting and
    //    11.60 % with the measured rates on.
    let rig_short_off = pick("rig", "eager/rotatinglast", "45s", "off");
    let rig_short_capture = pick("rig", "eager/rotatinglast", "45s", "capture");
    let rig_short_strangers = pick("rig", "eager/rotatinglast", "45s", "strangers");
    let to_phone = |outcome: &Outcome| {
        (outcome
            .share_of_top_freed_slots_to_churn()
            .expect("the top board spent a freed slot")
            * 100.0)
            .round() as u32
    };
    assert_eq!(
        (
            to_phone(rig_short_off),
            to_phone(rig_short_capture),
            to_phone(rig_short_strangers),
        ),
        (32, 1160, 40),
        "the rig room's freed slot moved off its measured shares (in hundredths of a per cent)"
    );

    // 3. And `disc` in a room with a phone in it is no longer zero at any
    //    lifetime that ends links: 0 of 1000 orders split with every dial
    //    connecting, 238 with the measured static rate. A death frees the
    //    slot and the re-dial no longer always lands, so the snapshot at
    //    the horizon catches boards mid-backoff — which is a real state a
    //    board is in for two to three rounds, not a settling artefact:
    //    these rows always ran to the horizon.
    for policy in ["eager/mostfree", "eager/rotatinglast"] {
        assert_eq!(
            pick("none", policy, "45s", "off").disconnected,
            0,
            "{policy}: 306's short-mode row split without a failure model"
        );
        assert!(
            pick("none", policy, "45s", "capture").disconnected >= 200,
            "{policy}: the short-mode row stopped splitting under the measured rate"
        );
    }
}

/// #412: 321's ledger rows re-measured with [`ConnectFailure`] on — the
/// column that says whether the instrument's downward bound mattered to
/// the ledger is `hold`.
#[test]
fn the_dial_that_fails_to_connect_re_measures_the_ledger() {
    println!("#412 — 321's ledger rows with a dial that can fail, per {ORDERS} orders.");
    println!(
        "{:<5} {:<6} {:>8} {:>10} {:>7} {:>7} {:>6} {:>7} {:>8} {:>7} {:>6} {:>5}",
        "room",
        "ledger",
        "life",
        "failure",
        "fail%",
        "hold",
        "hold%",
        "dials",
        "refused",
        "bb",
        "d/use",
        "disc"
    );
    let mut rows = Vec::new();
    for (statics, room) in [(Statics::NONE, "none"), (Statics::rig(1), "rig")] {
        for (ledger, label) in [(Ledger::NONE, "off"), (Ledger::after(3), "k=3")] {
            for (mortality, life) in LIFETIMES {
                for (failure, level) in FAILURES {
                    let outcome = measure(
                        3,
                        FallbackSpec::Eager,
                        TargetChoice::RotatingLast,
                        Churn::phones(1),
                        mortality,
                        statics,
                        ledger,
                        failure,
                    );
                    println!(
                        "{room:<5} {label:<6} {life:>8} {level:>10} {:>7} {:>7} {:>6} {:>7} \
                         {:>8} {:>7} {:>6} {:>5}",
                        Ratio(outcome.share_of_dials_that_failed()),
                        outcome.held_windows,
                        Ratio(outcome.share_of_held_windows_solo()),
                        outcome.dials,
                        outcome.refused,
                        outcome.board_links,
                        Ratio(outcome.dials_per_useful()),
                        outcome.disconnected,
                    );
                    rows.push(((room, label, life, level), outcome));
                }
            }
        }
    }
    let pick = |room: &str, label: &str, life: &str, level: &str| -> &Outcome {
        &rows
            .iter()
            .find(|((rr, rl, rlife, rf), _)| {
                *rr == room && *rl == label && *rlife == life && *rf == level
            })
            .expect("the row was measured above")
            .1
    };

    // The parameter is a parameter, on the ledger's own columns too.
    for (room, label, life, hold, dials, refused) in [
        ("none", "off", "immortal", 0, 9556, 4746),
        ("none", "k=3", "immortal", 6526, 7686, 2704),
        ("none", "k=3", "45s", 3160, 30_960, 2272),
        ("none", "k=3", "600s", 7104, 9314, 2706),
        ("rig", "k=3", "45s", 58, 31_588, 32),
        ("rig", "k=3", "600s", 482, 6570, 326),
    ] {
        let off = pick(room, label, life, "off");
        assert_eq!(
            (off.held_windows, off.dials, off.refused, off.failed),
            (hold, dials, refused, 0),
            "{room} {label} {life}: the failure model moved a 321 row"
        );
    }

    // 1. 321's own claim survives at the measured rates: the ledger costs
    //    the room no board-to-board link it would otherwise have had. In
    //    the phone room the two are EQUAL at every lifetime and every
    //    level; in the rig room the bound 321 states — 1 % of the room's
    //    links, `disc` no worse — still holds.
    for (_, life) in LIFETIMES {
        for (_, level) in FAILURES {
            assert_eq!(
                pick("none", "k=3", life, level).board_links,
                pick("none", "off", life, level).board_links,
                "none {life} {level}: the ledger cost the phone room a board link"
            );
            let with = pick("rig", "k=3", life, level);
            let without = pick("rig", "off", life, level);
            assert!(
                with.board_links * 100 >= without.board_links * 99
                    && with.disconnected <= without.disconnected + 4,
                "rig {life} {level}: the ledger cost the room more than 321's bound"
            );
        }
    }

    // 2. The instrument's bound was understating the ledger's work, not
    //    overstating it: on the immortal row the pause holds 6526 windows
    //    shut with every dial connecting and 7336 at the measured static
    //    rate.
    assert!(
        pick("none", "k=3", "immortal", "capture").held_windows
            > pick("none", "k=3", "immortal", "off").held_windows,
        "the measured failure rate did not give the ledger more to hold"
    );

    // 3. And its whole feed on the immortal rows really is the duplicate
    //    refusal, which the stranger level demonstrates by removing it:
    //    a phone no dial can reach is a phone no dial can be refused BY,
    //    so `refused` falls from 2704 to 230 and the ledger goes from
    //    6526 held windows to 24. 321 read that as a phase lock; this is
    //    the same fact from the other side.
    let strangers = pick("none", "k=3", "immortal", "strangers");
    let off = pick("none", "k=3", "immortal", "off");
    assert!(
        strangers.refused * 10 < off.refused && strangers.held_windows * 100 < off.held_windows,
        "a phone that cannot be connected to still fed the ledger"
    );
}

/// #412: #375's own guarantee cell — the room of ten and twenty boards
/// — read with a dial that can fail.
///
/// Two rooms only, and each for the reason the document reads it. The
/// EMPTY room is the guarantee itself (`disc` = 0, `bb` = 10 000 and
/// 20 000, `d/use` = 1.00), and it holds no rotating peer at all, so
/// [`ConnectFailure::CAPTURE`] and [`ConnectFailure::CAPTURE_STRANGERS`]
/// must be identical in every cell — asserted, and the model's own
/// positive control that the rotating half is the only thing that
/// separates them. The PHONE room is 306's ten- and twenty-board rows,
/// at the capture's short mode, where the freed slot has a denominator.
#[test]
fn the_dial_that_fails_to_connect_re_measures_the_board_rooms() {
    println!(
        "#412 — the rooms of ten and twenty boards with a dial that can fail, \
         per {ORDERS} orders. `bdls` counts boards left with no board link."
    );
    println!(
        "{:>3} {:<5} {:<19} {:>8} {:>10} {:>7} {:>7} {:>9} {:>6} {:>5} {:>5}",
        "n", "room", "policy", "life", "failure", "fail%", "top%", "bb", "d/use", "disc", "bdls"
    );
    let mut rows = Vec::new();
    for n in [10, 20] {
        for (churn, policy, choice, life, mortality) in [
            (
                Churn::NONE,
                "eager/rotatinglast",
                TargetChoice::RotatingLast,
                "immortal",
                Mortality::IMMORTAL,
            ),
            (
                Churn::NONE,
                "eager/rotatinglast",
                TargetChoice::RotatingLast,
                "45s",
                Mortality::CAPTURE_SHORT_MODE,
            ),
            (
                Churn::phones(1),
                "eager/mostfree",
                TargetChoice::MostFreeSlots,
                "45s",
                Mortality::CAPTURE_SHORT_MODE,
            ),
            (
                Churn::phones(1),
                "eager/rotatinglast",
                TargetChoice::RotatingLast,
                "45s",
                Mortality::CAPTURE_SHORT_MODE,
            ),
        ] {
            let room = if churn.peers == 0 { "empty" } else { "phone" };
            for (failure, level) in FAILURES {
                let outcome = measure(
                    n,
                    FallbackSpec::Eager,
                    choice,
                    churn,
                    mortality,
                    Statics::NONE,
                    Ledger::NONE,
                    failure,
                );
                println!(
                    "{n:>3} {room:<5} {policy:<19} {life:>8} {level:>10} {:>7} {:>7} {:>9} \
                     {:>6} {:>5} {:>5}",
                    Ratio(outcome.share_of_dials_that_failed()),
                    Ratio(outcome.share_of_top_freed_slots_to_churn()),
                    outcome.board_links,
                    Ratio(outcome.dials_per_useful()),
                    outcome.disconnected,
                    outcome.boardless,
                );
                rows.push(((n, room, policy, life, level), outcome));
            }
        }
    }
    let pick = |n: usize, room: &str, policy: &str, life: &str, level: &str| -> &Outcome {
        &rows
            .iter()
            .find(|((rn, rr, rp, rl, rf), _)| {
                *rn == n && *rr == room && *rp == policy && *rl == life && *rf == level
            })
            .expect("the row was measured above")
            .1
    };

    // The parameter is a parameter: #375's guarantee cell is untouched
    // with every dial connecting.
    for (n, bb) in [(10usize, 10_000usize), (20, 20_000)] {
        let off = pick(n, "empty", "eager/rotatinglast", "immortal", "off");
        assert_eq!(
            (
                off.board_links,
                off.disconnected,
                off.boardless,
                off.failed,
                off.dials_per_useful()
            ),
            (bb, 0, 0, 0, Some(1.0)),
            "n={n}: the failure model moved #375's guarantee cell"
        );
    }

    // 1. The empty room holds no rotating peer, so the two levels differ
    //    in nothing it can read: every cell equal, which is the model's
    //    own positive control that the rotating half of
    //    [`ConnectFailure`] is the only thing between them.
    for n in [10, 20] {
        for life in ["immortal", "45s"] {
            let capture = pick(n, "empty", "eager/rotatinglast", life, "capture");
            let strangers = pick(n, "empty", "eager/rotatinglast", life, "strangers");
            assert_eq!(
                (
                    capture.board_links,
                    capture.disconnected,
                    capture.boardless,
                    capture.dials,
                    capture.failed
                ),
                (
                    strangers.board_links,
                    strangers.disconnected,
                    strangers.boardless,
                    strangers.dials,
                    strangers.failed
                ),
                "n={n} {life}: the stranger level moved a room with nothing rotating in it"
            );
        }
    }

    // 2. The FORMATION phase survives it, and the reason is that a dial
    //    that failed is a dial the board makes again: at ten boards the
    //    room still forms all 10 000 links and still splits in no order
    //    at all, at 1.55 dials per link instead of 1.00. At twenty it
    //    stops being exactly zero — 14 orders in 1000 — so the guarantee
    //    cell is a cell measured under "every dial connects", not a
    //    property of the rule.
    let ten = pick(10, "empty", "eager/rotatinglast", "immortal", "capture");
    let twenty = pick(20, "empty", "eager/rotatinglast", "immortal", "capture");
    assert_eq!(
        (
            ten.board_links,
            ten.disconnected,
            twenty.board_links,
            twenty.disconnected
        ),
        (10_000, 0, 20_000, 14),
        "the formation phase moved off its measured cells"
    );

    // 3. The STEADY state does not survive it, and that is the largest
    //    single thing this parameter changes in the document: in a room
    //    of boards alone at the capture's short mode, `disc` goes from 0
    //    to 508 and 820 per 1000 orders and boards left with no board
    //    link from 0 to 280 and 416, with 13 % fewer board-to-board links
    //    formed. "`disc` is the connectivity column and it stays at 0 in
    //    a room of boards" is a sentence about a room where every dial
    //    connects.
    for (n, disc, boardless) in [(10usize, 508usize, 280usize), (20, 820, 416)] {
        let off = pick(n, "empty", "eager/rotatinglast", "45s", "off");
        let capture = pick(n, "empty", "eager/rotatinglast", "45s", "capture");
        assert_eq!(
            (off.disconnected, off.boardless),
            (0, 0),
            "n={n}: the short-mode empty room split without a failure model"
        );
        assert_eq!(
            (capture.disconnected, capture.boardless),
            (disc, boardless),
            "n={n}: the short-mode empty room moved off its measured split rate"
        );
        assert!(
            capture.board_links * 100 < off.board_links * 90,
            "n={n}: the failure rate stopped costing the room links"
        );
    }
}

/// #412: the failure model is a parameter, and each of the three things
/// it does is visible on its own.
///
/// The four controls the other parameters' control tests keep: the
/// mechanism fires, the zero is a true zero, the two halves are
/// separable, and a row that improves is explained rather than banked.
#[test]
fn control_the_connect_failure_model_is_a_parameter_and_every_mechanism_fires() {
    // 1. The zero is a true zero. In the room with the most dials in it,
    //    `ConnectFailure::NONE` fails not one and condemns not one
    //    address.
    let none = measure(
        3,
        FallbackSpec::Eager,
        TargetChoice::RotatingLast,
        Churn::phones(1),
        Mortality::CAPTURE_SHORT_MODE,
        Statics::rig(1),
        Ledger::NONE,
        ConnectFailure::NONE,
    );
    assert_eq!(
        (none.failed, none.failed_churn, none.failed_at_connect),
        (0, 0, 0),
        "a dial failed with the failure model off"
    );

    // 2. The static half fires at the rate it states, and splits between
    //    the two stages at the share it states. A room of boards alone
    //    has nothing but static addresses in it, so the room's own
    //    `fail%` IS the parameter — a positive control on the draw, not
    //    on the model.
    let boards_only = measure(
        10,
        FallbackSpec::Eager,
        TargetChoice::RotatingLast,
        Churn::NONE,
        Mortality::CAPTURE_SHORT_MODE,
        Statics::NONE,
        Ledger::NONE,
        ConnectFailure::CAPTURE,
    );
    let rate = boards_only
        .share_of_dials_that_failed()
        .expect("the room dialled");
    assert!(
        (rate - 35.0).abs() < 1.0,
        "the static rate drew {rate:.2} % against the 35.0 % it states"
    );
    let at_connect = boards_only.failed_at_connect as f64 * 100.0 / boards_only.failed as f64;
    assert!(
        (at_connect - 34.2).abs() < 1.0,
        "the connect-stage share drew {at_connect:.2} % against the 34.2 % it states"
    );

    // 3. The rotating half is separable from it, and it is the ADDRESS
    //    that carries the verdict: at `CAPTURE_STRANGERS` the phone is in
    //    the room, wins the fallback and is never once linked, while the
    //    same room at `CAPTURE` links it thousands of times. A dial still
    //    goes there — the order still elects it — which is what makes it
    //    a theft rather than an absence.
    let reachable = measure(
        3,
        FallbackSpec::Eager,
        TargetChoice::MostFreeSlots,
        Churn::phones(1),
        Mortality::CAPTURE_SHORT_MODE,
        Statics::NONE,
        Ledger::NONE,
        ConnectFailure::CAPTURE,
    );
    let stranger = measure(
        3,
        FallbackSpec::Eager,
        TargetChoice::MostFreeSlots,
        Churn::phones(1),
        Mortality::CAPTURE_SHORT_MODE,
        Statics::NONE,
        Ledger::NONE,
        ConnectFailure::CAPTURE_STRANGERS,
    );
    assert!(
        reachable.churn_links > 1000,
        "the reachable phone took no link at all"
    );
    // Not zero: the stranger level is the capture's own 95.6 % of
    // addresses, so about one rotation in twenty-three still lands on a
    // reachable one, and the three boards can each link to it while it
    // lasts. 148 links against 1768 — twelve times fewer, and every one
    // of them in the 4.4 % of rotations the capture also has.
    assert_eq!(
        (stranger.churn_links, reachable.churn_links),
        (148, 1768),
        "the stranger level moved off the link count its 95.6 % implies"
    );
    assert!(
        stranger.failed_churn > 1000,
        "the unreachable phone stopped taking dials, which is not what a stranger does"
    );

    // 4. And the improving row is explained rather than banked. At
    //    `CAPTURE_STRANGERS` the room's `refused` column collapses,
    //    which looks like the duplicate-refusal problem solving itself.
    //    It is not: a refusal needs a link, and a phone that cannot be
    //    connected to holds none of ours. The dials that used to end in a
    //    refusal end in a failure instead, and the room spends MORE of
    //    them.
    assert!(
        stranger.refused * 4 < reachable.refused && stranger.dials > reachable.dials,
        "the stranger row got cheaper instead of merely quieter"
    );
}

// ---------------------------------------------------------------------
// #353 — seed 2 of the rig's ble_room_10 cell: the lowest address
// arrives ninth and finds nine full tables
// ---------------------------------------------------------------------

/// Node `i`'s adapter address in the rig's seed-2 room
/// (`periculum run regression/ble_room_10.toml --seed 2`, saved log
/// `ble_room_10_2026-09-27T13-10-50Z.log`): btvirt hands out
/// `00:AA:01:0i:00:0(i+1)`, so the address order IS the node index
/// order and node 0 holds the room's lowest address — the one the v2.2
/// sort says must dial everyone and, strictly, is dialled by nobody.
/// Seed 2 shuffles the arrivals to `4,8,5,1,7,6,3,2,0,9`: node 0
/// arrives ninth, 4.0 s after the first board, into a room that is
/// busy saturating itself.
const fn seed2_addr(node: usize) -> u64 {
    0x00AA_0100_0000 + ((node as u64) << 16) + node as u64 + 1
}

/// The eighteen links the nine early arrivals formed, in formation
/// order: `(dialler, target, when)` with `when` in milliseconds after
/// 13:08:00Z, the central side's `BLE_LINK_UP` stamp from the saved
/// log. Every one is a strict sort win (asserted in the replay), none
/// ever went down (zero `BLE_LINK_DOWN` in 150 s), and node 0 is in
/// none of them. Node 1 — the lowest address among the nine — holds
/// FOUR central links: the strict sort makes the low addresses spend
/// the dials, which is what tells the two table shapes apart below.
const SEED2_DIALS: [(usize, usize, u32); 18] = [
    (4, 5, 16_501),
    (1, 2, 18_787),
    (7, 8, 19_073),
    (6, 7, 19_742),
    (5, 6, 19_884),
    (2, 4, 20_796),
    (3, 4, 22_563),
    (4, 6, 24_183),
    (8, 9, 25_277),
    (5, 7, 25_704),
    (1, 3, 25_861),
    (7, 9, 26_562),
    (3, 5, 26_806),
    (6, 8, 27_139),
    (2, 3, 28_426),
    (1, 8, 33_263),
    (2, 9, 35_052),
    (1, 9, 39_144),
];

/// The two link-table shapes a Columba room can be built of.
///
/// `Board` is the firmware's, and the one every `run_sim` claim in this
/// file — including "0 of 1000 disconnected at ten boards" — is about:
/// one central slot, [`PERIPH_SLOTS`] peripheral ones
/// ([`Board::outgoing`] / [`Board::incoming`] above). `CombinedFour`
/// is lnsd's: one cap over both roles (`links.rs:63
/// DEFAULT_MAX_LINKS = 4`, `is_full` at `links.rs:346`), any mix of
/// roles fills it.
///
/// Both stacks take a full table off the air (lnsd:
/// `should_advertise = !is_full`, `links.rs:359`; the firmware:
/// ADV_LOCK) and both refuse a surplus dial only AFTER a connection
/// exists — lnsd's `Admission::RejectFull` sits behind connect,
/// identity read and handshake. A dark peer sends no connectable
/// advertising PDU, so a dial at it can only run out its setup budget
/// (`bluez.rs:45 SETUP_TIMEOUT`, 20 s). The admission a dialler
/// actually meets is therefore the advertising rule: a dial lands iff
/// the target is on the air.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RoomTable {
    CombinedFour,
    Board,
}

#[derive(Clone, Copy, Default)]
struct RoomSlots {
    outgoing: usize,
    incoming: usize,
}

impl RoomTable {
    /// Whether a node with these slots is on the air.
    fn advertises(self, s: RoomSlots) -> bool {
        match self {
            RoomTable::CombinedFour => s.outgoing + s.incoming < 4,
            RoomTable::Board => s.incoming < PERIPH_SLOTS,
        }
    }

    /// Whether a node with these slots may open a dial. lnsd gates
    /// both the scan path and the dial pump on `is_full()`
    /// (`ble/mod.rs:617`, `:793`); a board's central task needs its
    /// one outgoing slot (a dark board may still dial — advertising
    /// reads the peripheral slots, dialling the central one).
    fn may_dial(self, s: RoomSlots) -> bool {
        match self {
            RoomTable::CombinedFour => s.outgoing + s.incoming < 4,
            RoomTable::Board => s.outgoing < 1,
        }
    }
}

/// Replay [`SEED2_DIALS`] against one table shape: a dial lands iff
/// the dialler may still dial and the target is still on the air, and
/// a landed dial spends the dialler's outgoing and the target's
/// incoming slot. Returns the final slots and which dials landed.
fn replay_seed2(table: RoomTable) -> ([RoomSlots; 10], [bool; 18]) {
    let mut slots = [RoomSlots::default(); 10];
    let mut landed = [false; 18];
    for (k, &(dialler, target, _)) in SEED2_DIALS.iter().enumerate() {
        assert_eq!(
            should_initiate(
                0,
                seed2_addr(dialler),
                Some(0),
                seed2_addr(target),
                ScanMode::Strict,
            ),
            ConnectDecision::InitiateLowerAddress,
            "the rig logged every one of these as a strict sort win"
        );
        if table.may_dial(slots[dialler]) && table.advertises(slots[target]) {
            slots[dialler].outgoing += 1;
            slots[target].incoming += 1;
            landed[k] = true;
        }
    }
    (slots, landed)
}

/// #353, first replay: seed 2's room on lnsd's combined table. All
/// eighteen dials are admissible and saturate the nine early arrivals
/// completely — 4-regular on nine vertices spends every one of their
/// 36 slot-instances — and the end state is ABSORBING: only node 0 is
/// on the air, so its window can never hold a candidate; the nine each
/// have the #375 escape verdict on node 0's advertisement (the rig
/// logged one `initiate_fallback` per node at 13:09:02–22) and every
/// one dies on the full-table gate; and no link ever dies to free a
/// slot. Node 0 is unreachable by rule, not by chance — which is what
/// the KNOWN-REDS attribution of this seed says.
///
/// This is the question the suite had never asked: every `run_sim`
/// claim above is about [`RoomTable::Board`], under which this room
/// cannot even be built (the test below). Whether lnsd keeps the
/// combined table, splits it like a board's, advertises its slot
/// count, or sheds a link for a linkless dialler is a design decision
/// (#353); until it lands, this test pins the mechanism.
#[test]
fn lnsds_combined_table_lets_seed_2s_nine_saturate_and_absorb_node_0() {
    let (slots, landed) = replay_seed2(RoomTable::CombinedFour);
    assert!(
        landed.iter().all(|&l| l),
        "the rig's room formed all eighteen links under the combined cap"
    );
    assert_eq!(
        (slots[0].outgoing, slots[0].incoming),
        (0, 0),
        "node 0 ended linkless (BLE_ROOM_LINKS node=0 links=0)"
    );
    for (i, s) in slots.iter().enumerate().skip(1) {
        assert_eq!(
            s.outgoing + s.incoming,
            4,
            "node {i} ended full (BLE_ROOM_LINKS links=4)"
        );
    }

    // The absorption, piece by piece. First: node 0 is the only node
    // on the air, and a scanner never sights itself, so node 0's
    // window is empty in EITHER mode — its last logged window was
    // 13:08:33, before the ninth adv gate closed.
    let on_air: Vec<usize> = (0..slots.len())
        .filter(|&i| RoomTable::CombinedFour.advertises(slots[i]))
        .collect();
    assert_eq!(on_air, [0], "after saturation only node 0 advertises");

    // Second: the nine are not even stuck for lack of a verdict. In
    // strict mode they wait (the run's 99 `wait_peer_lower_address`
    // lines about node 0), in fallback mode the #375 escape tells
    // them to dial node 0 — and the full-table gate (`ble/mod.rs:617`)
    // eats the dial. The escape hatch exists and is disabled by the
    // very saturation it would relieve.
    for (i, s) in slots.iter().enumerate().skip(1) {
        assert_eq!(
            should_initiate(0, seed2_addr(i), Some(0), seed2_addr(0), ScanMode::Strict),
            ConnectDecision::WaitPeerHasLowerAddress,
        );
        let escape = should_initiate(0, seed2_addr(i), Some(0), seed2_addr(0), ScanMode::Fallback);
        assert_eq!(escape, ConnectDecision::InitiateFallback);
        assert!(
            escape.initiate() && !RoomTable::CombinedFour.may_dial(*s),
            "node {i}: the fallback verdict fired and the full table gated it"
        );
    }

    // Third: node 0's own three dials against this timeline. The rig's
    // queue serialises one 20 s setup at a time, so the pop times lag
    // the elections by up to 30 s — and the room darkens meanwhile.
    let links_at = |node: usize, t: u32| {
        SEED2_DIALS
            .iter()
            .filter(|&&(d, g, when)| (d == node || g == node) && when <= t)
            .count()
    };
    // Dial 1 popped at 13:08:21.56 toward node 4, which was ON the air
    // with two free slots and stayed connectable until 13:08:24.18:
    // this dial lost a live 2.6 s race at the emulator/BlueZ layer,
    // not a structural one — the structure only guarantees the loss
    // from the adv gate onward.
    assert_eq!(links_at(4, 21_560), 2, "dial 1's target was not yet full");
    // Dials 2 and 3 popped at 13:08:42.38 and 13:09:04.37, and their
    // targets were full — and therefore dark, unable to refuse — from
    // 13:08:39: twenty seconds of silence each was the only possible
    // outcome.
    assert_eq!(links_at(1, 42_375), 4, "dial 2 popped against a dark peer");
    assert_eq!(links_at(9, 64_374), 4, "dial 3 popped against a dark peer");
}

/// #353, second replay: the same eighteen dials on the board table.
/// The room cannot be built — the first dial the one-outgoing-slot
/// shape refuses is node 4's SECOND central dial, at 13:08:24.18, the
/// moment the rig's room left everything this file's simulation had
/// ever measured — and the saturation trap does not exist: before the
/// tenth node holds a link, the nine others can spend at most eight
/// strict dials (upward) plus the top board's one fallback dial
/// (downward), nine incoming slots against a supply of 27, which
/// darkens at most two boards. Node 0 always has a candidate on the
/// air, and its first rig dial (toward node 4) simply lands.
#[test]
fn the_board_table_cannot_build_seed_2s_room_and_node_0s_first_dial_lands() {
    let (slots, landed) = replay_seed2(RoomTable::Board);
    let first_refused = landed
        .iter()
        .position(|&l| !l)
        .expect("the board table refuses the room");
    assert_eq!(
        (SEED2_DIALS[first_refused].0, SEED2_DIALS[first_refused].1),
        (4, 6),
        "the first refusal is node 4's second central dial"
    );
    assert_eq!(
        landed.iter().filter(|&&l| l).count(),
        8,
        "one outgoing each: eight of the eighteen dials came first"
    );
    for (i, s) in slots.iter().enumerate().skip(1) {
        assert!(s.outgoing <= 1, "node {i} kept the one-central shape");
        assert!(
            RoomTable::Board.advertises(*s),
            "node {i} is still on the air"
        );
    }
    // Node 0's first rig dial — the one that spent 20 s blind on the
    // combined table — finds its target advertising a free peripheral
    // slot and forms.
    assert!(
        RoomTable::Board.may_dial(slots[0]) && RoomTable::Board.advertises(slots[4]),
        "node 0 dials node 4 and node 4 can take it"
    );

    // And the worst case, constructed: an adversary spending every
    // outgoing slot the nine have on making boards dark reaches two
    // dark boards, not three — 3 incoming darken one board, the strict
    // sort only dials upward, and the top board's own dial is a
    // fallback dial downward, out of any dark set. Seven boards stay
    // on the air for the linkless tenth.
    let mut adv = [RoomSlots::default(); 10];
    let packing: [(usize, usize, ScanMode); 9] = [
        (1, 7, ScanMode::Strict),
        (2, 7, ScanMode::Strict),
        (3, 7, ScanMode::Strict),
        (4, 8, ScanMode::Strict),
        (5, 8, ScanMode::Strict),
        (6, 8, ScanMode::Strict),
        (7, 9, ScanMode::Strict),
        (8, 9, ScanMode::Strict),
        (9, 1, ScanMode::Fallback),
    ];
    for (dialler, target, mode) in packing {
        assert!(
            should_initiate(0, seed2_addr(dialler), Some(0), seed2_addr(target), mode).initiate(),
            "the packing only uses dials the rule grants"
        );
        assert!(
            RoomTable::Board.may_dial(adv[dialler]) && RoomTable::Board.advertises(adv[target]),
            "the packing only uses dials the tables admit"
        );
        adv[dialler].outgoing += 1;
        adv[target].incoming += 1;
    }
    let dark: Vec<usize> = (1..adv.len())
        .filter(|&i| !RoomTable::Board.advertises(adv[i]))
        .collect();
    assert_eq!(
        dark,
        [7, 8],
        "nine outgoing slots darken at most two boards"
    );
    assert!(
        (1..adv.len()).any(|i| {
            RoomTable::Board.advertises(adv[i])
                && should_initiate(0, seed2_addr(0), Some(0), seed2_addr(i), ScanMode::Strict)
                    .initiate()
        }),
        "the linkless lowest address still has a strict candidate on the air"
    );
}
