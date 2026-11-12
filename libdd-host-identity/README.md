# libdd-host-identity

Host identity detection for Datadog libraries: what container, entity, and
machine the current process runs as.

## Modules

- `entity_id`: container ID and entity ID from the cgroup file, plus the
  `DD_EXTERNAL_ENV` admission-controller variable; `get_entity_headers()`
  bundles all three into `datadog-container-id` / `datadog-entity-id` /
  `datadog-external-env` header pairs.
- `machine_id`: stable host machine ID as a hyphenated UUID, cached for the
  process lifetime, mirroring `pkg/util/uuid.GetUUID()` in the Go agent.
- `azure_app_services`: Azure App Services environment detection and the
  metadata it reports.

## Example

```rust
use libdd_host_identity::entity_id;

if let Some(container_id) = entity_id::get_container_id() {
    println!("Running in container: {}", container_id);
}
```

## Feature flags

- `cgroup_testing`: allows tests to override the cgroup file path via
  `entity_id::set_cgroup_file` (php uses this for end-to-end tests).
