# libdd-host-identity

Host identity detection for Datadog libraries: what container, entity, and
machine the current process runs as.

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
