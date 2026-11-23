#!/bin/sh
# Debian package post-install script (tauri.conf.json: bundle.linux.deb.postInstallScript).
#
# Lets TermWorker's built-in TFTP server listen on port 69, which Linux keeps for
# privileged programs, without running TermWorker as root. dpkg replaces the file
# on every upgrade, which drops the capability, so this runs again each time.
# A failure here (setcap missing, a file system without extended attributes) must
# not fail the installation: TermWorker then explains how to use another port.
set -e

if [ "$1" = "configure" ]; then
    if command -v setcap >/dev/null 2>&1; then
        setcap cap_net_bind_service=+ep /usr/bin/termworker ||
            echo "termworker: could not allow port 69 for the TFTP server (setcap failed); use a port above 1023" >&2
    else
        echo "termworker: setcap not found; the TFTP server will need a port above 1023" >&2
    fi
fi

exit 0
