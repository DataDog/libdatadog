# Remote Config through a Collector

An agentless client can route Remote Config through a Collector when the embedding
SDK enables OTel semantics and provides a separate HTTPS Remote Config origin:

```rust
use libdd_common::Endpoint;
use libdd_remote_config::fetch::AgentlessConfig;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let site = Endpoint::agentless("datadoghq.com", "api-key".to_string())?;
    let config = AgentlessConfig::new("application-host".to_string(), &site)?
        .with_otel_collector_endpoint(
            true,
            Some("https://collector.example:8234".parse()?),
        )?;
    Ok(())
}
```

Pass the resulting configuration through the existing `ConfigInvariants.agentless`
option. A disabled semantics flag or absent origin leaves the existing direct route
unchanged. The library does not read environment variables; the SDK resolves its
semantics setting and collector origin and supplies them explicitly.

The origin is a scheme and authority, with an optional trailing slash. It must use
HTTPS and cannot contain credentials, a path prefix, or query parameters. The
Collector must expose both `GET /api/v0.1/org` and
`POST /api/v0.1/configurations`; an OTLP endpoint alone does not provide these routes.

The original Datadog site still selects production, staging, or Gov TUF trust roots.
Changing the transport hostname does not change trust roots, organization checks,
configuration signatures, target validation, client identity, acknowledgements, or
polling behavior. The application retains its real API key, and the Collector
forwards it with the opaque protobuf payloads. TLS validation remains enabled.

This transport option does not implement Collector-side credential injection,
keyless SDK initialization, or the Agent's `/v0.7/config` protocol.
