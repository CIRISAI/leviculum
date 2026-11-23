--------------------------- MODULE MCChannelHold ---------------------------
(* Model values for ChannelHold.cfg: the ordinary waits grow as the code's *)
(* 1.5x backoff does, at the model's scale (100, 150, 225, 337, 506 ms).   *)
EXTENDS ChannelHold
OrdV == <<1, 2, 2, 3, 5>>
=============================================================================
