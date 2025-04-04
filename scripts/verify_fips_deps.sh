#!/bin/bash
# verify_fips_deps.sh
# Script to verify that the fips feature doesn't include ring and uses the proper crypto library
# Usage: ./verify_fips_deps.sh [package_name] (defaults to ddcommon if not specified)

set -e

# Default to ddcommon if no package is specified
PACKAGE=${1:-ddcommon}

echo "Checking ${PACKAGE} with fips feature..."

# Check if aws-lc-fips-sys is included with FIPS feature
FIPS_SYS_COUNT=$(cargo tree -p ${PACKAGE} --features fips | grep -c "aws-lc-fips-sys" || true)

if [ "$FIPS_SYS_COUNT" -eq 0 ]; then
  echo "❌ ERROR: aws-lc-fips-sys is not included when fips feature is enabled"
  exit 1
else
  echo "✅ aws-lc-fips-sys is correctly included with fips feature"
fi

# Check if ring is included with FIPS feature (should not be)
RING_COUNT=$(cargo tree -p ${PACKAGE} --features fips | grep -c "ring" || true)

if [ "$RING_COUNT" -eq 0 ]; then
  echo "✅ ring is correctly NOT included with fips feature"
else
  echo "❌ ERROR: ring is included when fips feature is enabled"
  exit 1
fi

echo "All checks passed! ${PACKAGE} FIPS feature doesn't include ring."
exit 0