--------------------------- MODULE MCChannelHold ---------------------------
(* Model values for ChannelHold.cfg: the cold waits grow as the code's     *)
(* 1.5x backoff does, at the model's scale (100, 150, 225, 337, 506 ms).   *)
EXTENDS ChannelHold
OrdV == <<1, 2, 2, 3, 5>>
\* Bound 30 x 1.5^(k-1) / 13.19, rounded down (wait_share_ms): sum 28.
ShareV == <<2, 3, 5, 7, 11>>
\* A cold ladder LONGER than its shares (an uncapped Karn SRTT), for the
\* cfg variant ChannelHoldCold.cfg: Bound 10 x 1.5^(k-1) / 13.19.
OrdColdV == <<3, 4, 5, 6, 8>>
ShareColdV == <<0, 1, 1, 2, 3>>
=============================================================================
