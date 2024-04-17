// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub use instance_id::InstanceId;
pub use queue_id::QueueId;
pub use request_identification::{RequestIdentification, RequestIdentifier};
pub use runtime_metadata::RuntimeMetadata;
pub use serialized_tracer_header_tags::SerializedTracerHeaderTags;
pub use session_info::SessionInfo;
pub use sidecar_interface::{
    SidecarInterface, SidecarInterfaceClient, SidecarInterfaceRequest, SidecarInterfaceResponse,
};

mod instance_id;
pub mod queue_id;
mod request_identification;
mod runtime_metadata;
mod serialized_tracer_header_tags;
mod session_info;
mod sidecar_interface;
