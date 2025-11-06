# libdd-dogstatsd-client

DogStatsD client library for sending metrics to Datadog.

## Overview

`libdd-dogstatsd-client` provides a client for sending metrics to Datadog via the DogStatsD protocol, an extension of StatsD with additional features like tags and histograms.
This client provides rust methods to interact with a dogstatsd server. It is mainly used in the `sidecar` and `data-pipeline crates`, but should be capable of being used elsewhere. See the crate docs for usage details.