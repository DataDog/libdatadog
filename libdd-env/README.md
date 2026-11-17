# libdd-env

Shared environment-variable parsing conventions for libdatadog crates.

## Overview

libdatadog crates that read configuration from the environment do so through
these helpers, so that conventions — most importantly which values count as
`true` for a boolean flag — are defined once and can evolve in a single place.

All helpers are one-shot reads: an unset, non-Unicode, or unparseable variable
yields `None`. A set-but-unexpected value is not `None` (a boolean flag holds
`false` for anything but case-insensitive `"true"`/`"1"`), keeping "unset"
and "explicitly set" distinguishable for callers that need the difference.
