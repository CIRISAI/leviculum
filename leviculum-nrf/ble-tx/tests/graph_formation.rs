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
//! ## What this harness still cannot see
//!
//! Four differences from a board remain, each stated with what it is
//! worth rather than ranked, because nothing here measures which of them
//! carries the residual:
//!
//! - **Every dial connects.** The captures say a board's dial usually
//!   does not: `feld-t114` logged 758 `BLE_CENTRAL_CONNECT` and 707
//!   `BLE_CENTRAL_FAIL`, so 93 % of its dials never came up;
//!   `feld-pocket` 49 of 107, `t114-boot` 48 of 164. A fallback dial that cannot connect goes into the
//!   dead-end table for two minutes (`BLE_DIAL_DEAD_END`, 50 times on
//!   `feld-t114`), which takes the board out of the next windows and
//!   leaves the phone. The harness only ever condemns an address after a
//!   duplicate-identity refusal.
//! - **A dead peer is back in the next round.** Death here is
//!   instantaneous and symmetric, and the ex-peer advertises again 5 s
//!   later at its full free-slot count. On a board the two ends learn of
//!   it up to 45 s apart (the central's supervision timeout is 4 s,
//!   `conn_params.rs`; the keepalive expiry is `LINK_TIMEOUT_MS`), and a
//!   peer that died by rebooting is not advertising at all for its boot
//!   time.
//! - **The room is boards and phones only.** The rig's room had a
//!   fourth member the model has no kind for: a static-addressed,
//!   central-capable host (identity `e19b2b38`, one address
//!   `d916e2923ed2`, the only one it ever advertised in 2417 lines) that
//!   took 27 of `feld-t114`'s 37 refill links across the captures, and
//!   every one of the 25 outgoing links `feld-t114` made after
//!   2026-09-25, when the shipped order went on the boards. It
//!   advertises no slot count, so it deficits by zero like a phone and
//!   then beats the phone on the rotating-last term — which is how the
//!   shipped order emptied `feld-t114`'s phone column on the rig while
//!   the harness's three-board room says it cannot. A static
//!   always-free peer is the first thing to add to this model.
//! - **Every advertiser is heard in every window.** One round is one
//!   complete window here. The firmware collects
//!   `SCAN_WINDOW_COLLECT_MS` (3 s) out of a 5 s cycle and can miss a board advertising on a 1-2 s
//!   cadence, while a phone advertising sub-second is heard nearly
//!   always.

use leviculum_ble_tx::{
    dial_preference, free_slots, judge_duplicate, should_initiate, with_free_slots, CandidateTable,
    ConnectDecision, DialPreference, DupVerdict, FallbackOrder, Origin, ScanMode,
    CAP_PERIPHERAL_ONLY, LINK_ABANDONED_MS, LINK_TIMEOUT_MS, MIN_USABLE_MTU,
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
}

impl TargetChoice {
    /// Whether the boards in this configuration advertise their free
    /// incoming slots (#375 item 3).
    fn advertises_slots(self) -> bool {
        matches!(
            self,
            Self::MostFreeSlots | Self::FallbackFirstHeard | Self::RotatingLast
        )
    }

    /// The tie-break the shared table uses for the fallback class.
    fn fallback_order(self) -> FallbackOrder {
        match self {
            Self::FallbackFirstHeard => FallbackOrder::FirstHeard,
            Self::RotatingLast => FallbackOrder::RotatingLast,
            _ => FallbackOrder::Address,
        }
    }

    /// Whether the row reads the order the advertising PDUs arrived
    /// in. Only the two orders that break a tie by it draw the shuffle
    /// — a row that never reads it must not consume the draw either,
    /// or the two would not be the same replay.
    fn reads_arrival_order(self) -> bool {
        matches!(self, Self::FallbackFirstHeard | Self::RotatingLast)
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
}

/// What a run spent and what it got, per #412's three numbers.
///
/// `dials` counts every dial that reached the identity read, which is
/// where a spent one is recognised; `dials == board_links + churn_links
/// + refused` is checked at the end of every run.
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
    /// Dials refused post-connect because the identity was already
    /// live — the rotated-address duplicate, spent by definition.
    refused: usize,
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
}

impl Tally {
    /// Dials that produced a link that lasted.
    fn useful(&self) -> usize {
        self.board_links + self.churn_links - self.short
    }
}

struct Sim {
    boards: Vec<Board>,
    tally: Tally,
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
fn run_sim(
    n: usize,
    seed: u64,
    spec: FallbackSpec,
    choice: TargetChoice,
    churn: Churn,
    mortality: Mortality,
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
    let mut churners: Vec<Churner> = (0..churn.peers)
        .map(|_| {
            let addr = (next_rand(&mut churn_rng) & 0x3FFF_FFFF_FFFF) | 0x4000_0000_0000;
            Churner {
                addr,
                identity: identity_from(addr),
                phase: (next_rand(&mut churn_rng) as u32) % rounds(CHURN_ROTATE_MS),
                links: Vec::new(),
            }
        })
        .collect();

    let mut tally = Tally::default();
    let horizon = if churn.peers == 0 && !mortality.kills() {
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
            boards[link.peer].incoming.retain(|l| l.peer != i);
            boards[link.peer].strict_rounds = 0;
            tally.deaths += 1;
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
            // The quiet spec suspends the clock while ANY link is live
            // (part 2's firmware reset it on every scan pass that found
            // a live connection); an outgoing link already stopped the
            // scan above, so incoming links are what decides here.
            let suspended = spec == FallbackSpec::Quiet && !boards[i].incoming.is_empty();
            let mode = if spec != FallbackSpec::Off
                && !suspended
                && boards[i].strict_rounds >= FALLBACK_AFTER_ROUNDS
            {
                ScanMode::Fallback
            } else {
                ScanMode::Strict
            };
            // Visible: arrived, advertising (a full board is not), not
            // ourselves, not already linked to us, not backed off; then
            // the real rule. A churning peer is always advertising and
            // never full — that is what "always in the room" means.
            let candidates: Vec<(usize, ConnectDecision, Option<u8>)> = (0..n + churn.peers)
                .filter_map(|p| {
                    if p == i {
                        return None;
                    }
                    let (addr, caps, free) = if p < n {
                        if !boards[p].arrived || boards[p].incoming.len() >= PERIPH_SLOTS {
                            return None;
                        }
                        let free = choice.advertises_slots().then(|| {
                            u8::try_from(PERIPH_SLOTS - boards[p].incoming.len())
                                .expect("slots fit a byte")
                        });
                        (boards[p].addr, Some(0), free)
                    } else {
                        // No v0.3.0 record at all: full capability per
                        // §3.2, no slot count per item 3.
                        (churners[p - n].addr, None, None)
                    };
                    if boards[i].addr_linked(addr) || boards[i].dead_end(addr, round) {
                        return None;
                    }
                    let decision = should_initiate(0, boards[i].addr, caps, addr, mode);
                    decision.initiate().then_some((p, decision, free))
                })
                .collect();
            if candidates.is_empty() {
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
                // policy, every board stating its free slots.
                TargetChoice::LowestEligible
                | TargetChoice::MostFreeSlots
                | TargetChoice::FallbackFirstHeard
                | TargetChoice::RotatingLast => {
                    let mut window: CandidateTable<usize, WINDOW_CANDIDATES> =
                        CandidateTable::with_fallback_order(choice.fallback_order());
                    // The order the advertising PDUs arrive in. It is
                    // the candidate order for every row that does not
                    // read it, and a seeded shuffle for the one that
                    // does. The shuffle has a stream of ITS OWN, a
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
                    for &(p, decision, free) in &offers {
                        let addr = if p < n {
                            boards[p].addr
                        } else {
                            churners[p - n].addr
                        };
                        // 303's tier, censused rather than assumed: the
                        // public rule is asked the same question the
                        // window's key asks it, per offer.
                        tally.offers += 1;
                        if dial_preference(decision, free) == DialPreference::CannotDialUs {
                            tally.offers_cannot_dial += 1;
                        }
                        window.offer(addr, decision, free, p);
                    }
                    window
                        .into_best()
                        .map(|(_, _, p)| p)
                        .expect("a non-empty candidate set chooses")
                }
            };
            // The dial. From here on the connection exists, so the
            // strict phase restarts in either outcome (the firmware's
            // `conn_link_up`), and the identity read decides whether
            // anything was gained by it.
            tally.dials += 1;
            boards[i].strict_rounds = 0;
            // The steady-state denominator: this board has held an
            // outgoing link before, so the slot it is spending now is
            // one that was freed.
            if boards[i].held_outgoing {
                tally.refill_dials += 1;
                if target >= n {
                    tally.refill_dials_churn += 1;
                }
                if i == top_board {
                    tally.refill_dials_top += 1;
                    if target >= n {
                        tally.refill_dials_top_churn += 1;
                        if !candidates.iter().any(|&(p, _, _)| p < n) {
                            tally.refill_dials_top_churn_solo += 1;
                        }
                    }
                }
            }
            if target < n {
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
            let churner = &mut churners[target - n];
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
                        tally.refused += 1;
                        let addr = churner.addr;
                        boards[i].note_dead_end(addr, round);
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

        linkless_streak = if any_link { 0 } else { linkless_streak + 1 };
        // Quiescent: everyone has arrived and even the boards that
        // reached fallback during the streak found nobody. Visibility
        // only changes when a link forms (a quiet-suspended board's
        // links never drop here), so nothing changes hereafter. With a
        // churning peer nothing is ever quiescent — rotations keep
        // arriving — so the run goes to the horizon instead.
        if churn.peers == 0
            && !mortality.kills()
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
    for board in &boards {
        if let Some(link) = board.outgoing {
            if (link.peer >= n || mortality.kills()) && link.useful_ms(horizon) < USEFUL_SESSION_MS
            {
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
    assert_eq!(
        tally.dials,
        tally.board_links + tally.churn_links + tally.refused,
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
    /// Dials that produced a link that lasted at least
    /// [`USEFUL_SESSION_MS`], summed — #412's third number is
    /// `dials / useful`.
    useful: usize,
    /// Of the spent ones: refused as a live duplicate identity.
    refused: usize,
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
    /// The same pair for the highest-addressed board alone — the board
    /// the rig's 7 of 7 is about — and how many of those dials had no
    /// board among the permitted candidates at all.
    refill_dials_top: usize,
    refill_dials_top_churn: usize,
    refill_dials_top_churn_solo: usize,
    /// Window offers and 303's tier among them, summed.
    offers: usize,
    offers_cannot_dial: usize,
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

    /// Of the top board's freed slots that went to a churning peer: what
    /// share had no board on offer at all. Where this is 100 %, no
    /// candidate ORDER can defend the slot — there is nothing to order.
    fn share_of_top_churn_dials_with_no_board_on_offer(&self) -> Option<f64> {
        (self.refill_dials_top_churn > 0).then(|| {
            self.refill_dials_top_churn_solo as f64 * 100.0 / self.refill_dials_top_churn as f64
        })
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

fn measure(
    n: usize,
    spec: FallbackSpec,
    choice: TargetChoice,
    churn: Churn,
    mortality: Mortality,
) -> Outcome {
    let mut outcome = Outcome {
        disconnected: 0,
        linkless: 0,
        saturated: 0,
        boardless: 0,
        board_links: 0,
        dials: 0,
        churn_links: 0,
        useful: 0,
        refused: 0,
        short: 0,
        deaths: 0,
        refill_dials: 0,
        refill_dials_churn: 0,
        refill_dials_top: 0,
        refill_dials_top_churn: 0,
        refill_dials_top_churn_solo: 0,
        offers: 0,
        offers_cannot_dial: 0,
    };
    for seed in 0..ORDERS {
        let sim = run_sim(n, 0xB1E5_0000 + seed, spec, choice, churn, mortality);
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
        outcome.useful += sim.tally.useful();
        outcome.refused += sim.tally.refused;
        outcome.short += sim.tally.short;
        outcome.deaths += sim.tally.deaths;
        outcome.refill_dials += sim.tally.refill_dials;
        outcome.refill_dials_churn += sim.tally.refill_dials_churn;
        outcome.refill_dials_top += sim.tally.refill_dials_top;
        outcome.refill_dials_top_churn += sim.tally.refill_dials_top_churn;
        outcome.refill_dials_top_churn_solo += sim.tally.refill_dials_top_churn_solo;
        outcome.offers += sim.tally.offers;
        outcome.offers_cannot_dial += sim.tally.offers_cannot_dial;
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
        let at10 = measure(10, spec, choice, Churn::NONE, Mortality::IMMORTAL);
        let at20 = measure(20, spec, choice, Churn::NONE, Mortality::IMMORTAL);
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
            let at10 = measure(10, spec, choice, churn, Mortality::IMMORTAL);
            let at20 = measure(20, spec, choice, churn, Mortality::IMMORTAL);
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
    );
    let strict_churned = measure(
        10,
        FallbackSpec::Off,
        TargetChoice::MostFreeSlots,
        Churn::advertisers(1),
        Mortality::IMMORTAL,
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
    );
    let strict_dialled = measure(
        10,
        FallbackSpec::Off,
        TargetChoice::FirstSeen,
        Churn::phones(1),
        Mortality::IMMORTAL,
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
                let outcome = measure(n, FallbackSpec::Eager, choice, churn, mortality);
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
    );
    let mortal = measure(
        10,
        FallbackSpec::Eager,
        TargetChoice::RotatingLast,
        Churn::NONE,
        Mortality::CAPTURE_SHORT_MODE,
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
