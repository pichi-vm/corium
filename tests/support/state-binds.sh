#!/usr/bin/sh
# SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
# SPDX-License-Identifier: Apache-2.0

set -eu
export PATH=/usr/bin:/usr/sbin
[ "$(findmnt -n -o FSTYPE /)" = ext4 ]
for path in var home root srv; do
    [ "$(findmnt -n -o FSTYPE /$path)" = ext4 ]
    [ ! -e /tmp/$path ]
done
[ "$(stat -c %a /root)" = 700 ]
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
    [ "$(stat -c %u:%g /var/state-marker)" = 123:456 ]
    [ ! -e /var/deleted ]
    for path in /home /root /srv; do
        [ "$(cat "$path/marker")" = persistent ]
        [ "$(stat -c %a "$path/marker")" = 600 ]
        [ "$(stat -c %u:%g "$path/marker")" = 123:456 ]
        [ ! -e "$path/deleted" ]
    done
    echo CORIUM-STATE-RETAINED
else
    [ "$(cat /var/lib/service/data)" = seed ]
    echo retained > /var/lib/service/data
    echo local > /var/local-metadata
    echo persistent > /var/state-marker
    chmod 600 /var/state-marker
    chown 123:456 /var/state-marker
    echo temporary > /var/deleted
    rm /var/deleted
    for path in /home /root /srv; do
        echo persistent > "$path/marker"
        chmod 600 "$path/marker"
        chown 123:456 "$path/marker"
        echo temporary > "$path/deleted"
        rm "$path/deleted"
    done
    echo CORIUM-STATE-NEW
fi
