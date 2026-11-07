#!/usr/bin/sh
# SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
# SPDX-License-Identifier: Apache-2.0

set -eu
export PATH=/usr/bin:/usr/sbin
[ "$(findmnt -n -o FSTYPE /)" = ext4 ]
[ "$(findmnt -n -o FSTYPE /var)" = ext4 ]
[ ! -e /tmp/var ]
[ ! -e /run/pichi/state ]
for path in / /etc /usr /var/lib/image; do
    if (echo forbidden > "$path/corium-forbidden") 2>/dev/null; then exit 1; fi
done
if (echo forbidden > /var/lib/image-version) 2>/dev/null; then exit 1; fi
[ "$(cat /var/lib/image/value)" = "$(cat /etc/image-version)" ]
[ "$(cat /var/lib/image-version)" = "$(cat /etc/image-version)" ]
[ "$(stat -c %a /var/lib/service)" = 700 ]
[ "$(stat -c %a /var/lib/service/data)" = 600 ]
[ -L /var/run ]
case "$(cat /etc/image-version)" in
    B)
        [ "$(cat /var/local-metadata)" = image ]
        if (echo forbidden > /var/local-metadata) 2>/dev/null; then exit 1; fi
        ;;
    C) [ "$(cat /var/local-metadata)" = local ] ;;
esac
if [ -f /var/state-marker ]; then
    [ "$(cat /var/lib/service/data)" = retained ]
    [ "$(cat /var/state-marker)" = persistent ]
    [ "$(stat -c %a /var/state-marker)" = 600 ]
    [ ! -e /var/deleted ]
    echo CORIUM-STATE-RETAINED
else
    [ "$(cat /var/lib/service/data)" = seed ]
    echo retained > /var/lib/service/data
    echo local > /var/local-metadata
    echo persistent > /var/state-marker
    chmod 600 /var/state-marker
    echo temporary > /var/deleted
    rm /var/deleted
    echo CORIUM-STATE-NEW
fi
