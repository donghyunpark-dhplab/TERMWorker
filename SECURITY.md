# Security

Please report security problems privately to **security@dhplab.com**
(also published at https://dhplab.com/.well-known/security.txt). Do not open a public GitHub issue for them.

Include what you found, how to reproduce it, and the TermWorker version or commit. Korean or English is fine.

TermWorker is maintained on a best-effort basis by DHPLab Consulting. Reports are read and answered as soon
as possible; there is no guaranteed response time. Fixes go into `main` and the next release.

Areas that matter most: the SSH client (host-key checks, authentication), file transfers (received names,
paths), the built-in TFTP server (paths, uploads) and the network adapter settings (the commands run with
administrator rights).
