# SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
# SPDX-License-Identifier: Apache-2.0

# Metadata-only ELF fixture: not an executable or loadable kernel module.
.section .modinfo,"a"
.asciz "license=Apache-2.0"
.asciz "description=Corium module metadata fixture"
.asciz "depends=reed_solomon,dm-mod"

# Regenerate from the repository root:
# cc -c -o tests/data/module.ko tests/data/module.S
