#!/bin/bash
# verify_fips_deps.sh
# Script to verify that the fips feature doesn't include ring and uses the proper crypto library
# Usage: ./verify_fips_deps.sh [package_name] [additional_features]
# Examples:
#   ./verify_fips_deps.sh                          # Checks ddcommon with fips feature
#   ./verify_fips_deps.sh datadog-trace-utils      # Checks trace-utils with fips feature
#   ./verify_fips_deps.sh datadog-trace-utils compression  # Checks trace-utils with fips,compression features

set -e

# Default to ddcommon if no package is specified
PACKAGE=${1:-ddcommon}
shift 2>/dev/null || true

# Additional features to include
ADDITIONAL_FEATURES="$@"
FEATURES="fips"

# Add additional features if specified
if [ -n "$ADDITIONAL_FEATURES" ]; then
  FEATURES="$FEATURES,$ADDITIONAL_FEATURES"
fi

echo "Checking ${PACKAGE} with features: ${FEATURES}..."

# Check if aws-lc-fips-sys is included
FIPS_SYS_COUNT=$(cargo tree -p ${PACKAGE} --features ${FEATURES} | grep -c "aws-lc-fips-sys" || true)

if [ "$FIPS_SYS_COUNT" -eq 0 ]; then
  echo "❌ ERROR: aws-lc-fips-sys is not included when fips feature is enabled"
  exit 1
else
  echo "✅ aws-lc-fips-sys is correctly included with features: ${FEATURES}"
fi

# Check if ring is included (should not be for runtime dependencies)
RING_COUNT=$(cargo tree -p ${PACKAGE} --features ${FEATURES} -e=no-dev -i ring | grep -c "ring" || true)

if [ "$RING_COUNT" -eq 0 ]; then
  echo "✅ ring is correctly NOT included with features: ${FEATURES} (in runtime dependencies)"
else
  echo "❌ ERROR: ring is included with features: ${FEATURES} (in runtime dependencies)"
  cargo tree -p ${PACKAGE} --features ${FEATURES} --no-dev-dependencies -i ring
  exit 1
fi

echo "All checks passed! ${PACKAGE} with features ${FEATURES} doesn't include ring in runtime dependencies."
exit 0
