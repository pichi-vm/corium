#!/usr/bin/sh
# SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
# SPDX-License-Identifier: Apache-2.0

set -eu
PATH=/usr/bin

# This fixture has one scute. Find its shared snapshot independently of the
# device that Corium mounted, so the old boot path reproduces EBUSY here.
origin=
for path in /sys/class/block/dm-*/dm/name; do
    read -r name < "$path"
    case "$name" in
        CARAPACE-s*)
            test -z "$origin"
            shared=$name
            read -r origin < "${path%/dm/name}/dev"
            ;;
    esac
done
test -n "$origin"
set -- $(dmsetup table "$shared")
sectors=$2
mkdir /run/root-derivation
truncate -s 32M /run/root-derivation/cow
backing=$(losetup --find --show /run/root-derivation/cow)
cleanup() {
    umount /run/root-derivation/mount 2>/dev/null || :
    dmsetup remove --noudevsync corium-test-derived 2>/dev/null || :
    losetup --detach "$backing" 2>/dev/null || :
}
trap cleanup EXIT

# A filesystem claim on the shared snapshot prevents this table from loading.
dmsetup create --noudevsync corium-test-derived \
    --table "0 $sectors snapshot $origin $backing P 8"
set -- $(dmsetup table corium-root)
test "$2" = "$sectors"
test "$3" = linear
test "$4" = "$origin"
test "$5" = 0
mkdir /run/root-derivation/mount
set -- $(dmsetup info -c --noheadings -o minor corium-test-derived)
mount -t ext4 -o nodev,nosuid "/dev/dm-$1" /run/root-derivation/mount
printf 'derived\n' > /run/root-derivation/mount/fixture/derived-marker
test ! -e /fixture/derived-marker
read -r marker < /fixture/source-marker
test "$marker" = original
umount /run/root-derivation/mount
dmsetup remove --noudevsync corium-test-derived
losetup --detach "$backing"
trap - EXIT
dmsetup info "$shared" >/dev/null
read -r marker < /fixture/source-marker
test "$marker" = original
printf 'CORIUM-SHARED-ROOT-OK\n'
