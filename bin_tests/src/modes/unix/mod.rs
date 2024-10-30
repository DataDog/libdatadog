// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
pub mod test_000_donothing;
pub mod test_001_sigpipe;
pub mod test_002_sigchld;
pub mod test_003_sigchld_with_exec;
pub mod test_004_donothing_sigstack;
pub mod test_005_sigpipe_sigstack;
pub mod test_006_sigchld_sigstack;
pub mod test_007_chaining;
