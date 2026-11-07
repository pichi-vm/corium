#!/usr/bin/sh
# SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
# SPDX-License-Identifier: Apache-2.0

set -eu
for path in /root-readonly /etc/root-readonly /usr/root-readonly; do
    if (echo bad > "$path") 2>/dev/null; then exit 1; fi
done
test ! -e /tmp/var
test ! -e /run/pichi/state
if test ! -e /var/phase; then
    echo retained > /var/marker
    chmod 600 /var/marker
    echo remove > /var/deleted
    echo 1 > /var/phase
    echo CORIUM-STATE-NEW
else
    test "$(cat /var/marker)" = retained
    test "$(stat -c %a /var/marker)" = 600
    if test "$(cat /var/phase)" = 1; then
        rm /var/deleted
        echo 2 > /var/phase
    else
        test ! -e /var/deleted
    fi
    echo CORIUM-STATE-RETAINED
fi
