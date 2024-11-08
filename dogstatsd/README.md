# DogStatsD

Provides a DogStatsD implementation which uses [Saluki](https://github.com/DataDog/saluki) for distribution metrics.

## Status
This project is in beta and possible frequent changes should be expected. It's primary purpose is for Serverless to send metrics from AWS Lambda Functions, Azure Functions, and Azure Spring Apps. It is still considered unstable for general purposes.

- No UDS support
- Uses `ustr`, so prone to memory leaks
- Arbitrary constraints in https://github.com/DataDog/libdatadog/blob/main/dogstatsd/src/constants.rs

## Additional Notes

Upstreamed from [Bottlecap](https://github.com/DataDog/datadog-lambda-extension/tree/main/bottlecap)
