# libdd-ddsketch

Minimal implementation of Datadog's DDSketch for accurate quantile estimation.

## Overview

DDSketch is a data structure for tracking value distributions with guaranteed relative error bounds using constant memory.

## Main Features

- **Data Insertion**: Add values to sketches with optional counts/weights
- **Count Queries**: Get total count of points in the sketch
- **Protobuf Serialization**: Encode sketches for transmission to Datadog backend

## References

- [DDSketch Paper](https://arxiv.org/abs/1908.10693)
- [Reference Implementation](https://github.com/DataDog/sketches-go)
